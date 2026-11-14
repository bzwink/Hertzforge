# Suite Protocol, Version `sds/1`

The one contract between the shell (TypeScript), the analysis service (Python) and the control
service (Rust). `protocol.schema.json` beside this file is the machine-readable form; when this
text and the schema disagree, fix one of them in the same change. Golden frames in `golden/` are
the shared test data every language's codec must reproduce.

`sds` is a working name. Renaming it is a mechanical find and replace.

## Transport

WebSocket, one connection per service, loopback by default. The control service listens on
loopback only (see "Controller and Observers").

| Service  | Default address           | Started by                              |
|----------|---------------------------|-----------------------------------------|
| analysis | `ws://127.0.0.1:8791/`    | `hertzforge serve --port 8791` |
| control  | `ws://127.0.0.1:8792/`    | `cargo run -p sds-control -- --port 8792` |
| shell    | `http://127.0.0.1:5191/`  | `npm --prefix ui run dev -- --port 5191` |

On connect the **server** sends a `hello` first. The client compares `protocol` for an exact
match and closes the connection on any difference (no graceful degradation across versions).

## Frames

- **Text frame:** one UTF-8 JSON message that carries no arrays.
- **Binary frame:** one message that carries arrays. Layout, all integers little-endian:

| Offset | Size | Content |
|--------|------|---------|
| 0 | 4 | ASCII magic `SDS1` |
| 4 | 4 | `u32` header length `H` in bytes |
| 8 | `H` | UTF-8 JSON header (the message), right-padded with spaces (0x20) so `8 + H` is a multiple of 8 |
| `8 + H` | rest | Payload: the arrays, each starting at a multiple of 8 from the payload start |

The header is a normal message plus a top-level `"arrays"` list. Each entry:
`{"id": "a0", "dtype": "f64", "shape": [n, ...], "offset": o, "nbytes": b}`. `offset` is counted
from the payload start (`8 + H`), is a multiple of 8, and `nbytes` equals the element count times
the element size. Data is C order (row-major), little-endian.

Inside the message, an array value is the object `{"$array": "<id>"}`. A decoder replaces each
reference with a typed array (TS: `Float64Array` etc., Python: `numpy.ndarray`, Rust: `Vec<T>`
plus shape) and drops the `arrays` list. Every listed array is referenced exactly once.

`dtype` is one of `f32`, `f64`, `i32`, `u32`, `u8`. Complex data travels as two real arrays
(`re`, `im`) by convention; there is no complex dtype.

## Messages

```
hello     {"type":"hello","protocol":"sds/1","service":"analysis"|"control","version":"0.1.0"}
request   {"type":"request","id":<int>,"method":"<name>","params":{...}}
response  {"type":"response","id":<int>,"result":{...}}
          {"type":"response","id":<int>,"error":{"code":"<code>","message":"<text>",
                                                  "reason":"<name>","detail":{...}}}
event     {"type":"event","topic":"<name>","seq":<int>,"data":{...}}
```

Error codes: `bad_request`, `unknown_method`, `not_found`, `refused`, `internal`. The message
names the cause in words a user can act on; an error never comes back as an empty result.
`reason` (optional) names the cause for a program: a stable snake_case name a client branches
on, so no client ever parses `message`. `detail` (optional) carries the facts behind it. Every
error from the hardware and Random methods carries a `reason` (see "Refusals" below).

## Views: The Viz Kit's Input

Every result and every live event carries `views`, a list the shell renders with its shared viz
kit. Services never send pixels and the shell never computes the numbers.

**`spectrum`** (also used for any x-y line data)

```
{"view":"spectrum","title":"...",
 "x":{"label":"Frequency","unit":"Hz","scale":"log"|"linear"},
 "y":{"label":"PSD","unit":"g²/Hz","scale":"log"|"linear"},
 "series":[{"name":"...","role":"measured"|"reference"|"tolerance"|"abort"|"limit"|"drive"|"other",
            "x":{"$array":..},"y":{"$array":..},
            "shape":"line"|"steps"|"law", "edges":{"$array":..}, "follows":0}],
 "link":"frequency",
 "regions":[{"x0":0.5,"x1":0.9,"kind":"frame"|"cut","label":"..."}],
 "handles":[{"series":0,"x":100.0,"decibels":3.0,"label":"...","op":"...","params":{...}}]}
```

`role` sets the line style so no meaning rests on color alone: measured solid, reference
dashed, tolerance dotted, abort dash-dot, limit dash-dot-dot, drive thin solid. `limit` is a
ceiling the controller holds a channel under; it never aborts. `link` (optional) names the cursor
group; panes with the same `link` share a cursor position on x.

`regions` (optional) shade spans of x behind the curves: a `frame` is one frame its spectra are averaged from (frames that
overlap shade darker where they do), and a `cut` is a span left out, such as the ends a truncation
discards; a `span` draws its edges alone. `label` is said when the span is hovered. A region with
`"drag":{"op","params","low","high"?}` is moved on the plot: its middle slides it and, with
`high`, an edge moves that edge. On release the shell runs `op` with `params`, the new x0 as
`low` and the new x1 as `high`, and the answer's views take the result's place. They are not listed in the legend; the
result's reading says what they are.

`handles` (optional) are points dragged up and down on the plot, each at `x` on the curve of
`series`, `decibels` from its target and said as `label`. On release the shell runs `op` with
`params` and the new `decibels`, and the answer's views take the result's place, as a
specification's band edges are dragged (`project.set_band_edge`).

`follows` (optional) is the index, in this view's `series`, of the measurement a curve follows:
a model's prediction of it, such as an FRF synthesized from fitted modes. A follower is drawn
dashed in its measurement's color, above it, and left out of the legend. It names an earlier series that is not itself a follower.

`shape` (optional, default `line`) says how the points mean to be drawn, and the service that
computed the data decides it. Spectral content is plotted the way it is calculated:

- `steps`: each `y` is a value over its own bin and is drawn flat across it, from `edges[i]`
  to `edges[i+1]`, so the area drawn is the area a PSD integrates to. `edges` is required,
  ascending, one longer than `y`. For FFT lines the edges sit midway between lines and the
  outer edges mirror the nearest midpoints, so every bin is `df` wide; banded data sends its
  own band edges. A cursor reads the bin it is over.
- `law`: a breakpoint specification, already resampled along its power law by the service so a
  polyline is within a pixel of it. Drawn as a line.
- `line`: anything that is a sample at a point (an FRF, a coherence, a time history).

`groups` (optional) shows the series a set at a time, chosen from a list over the plot: a
channel each, say, with its target and its measurement. Each group names its series by index into
`series` and may carry `readouts`, `{"label","text","status"}` readings shown beside the list while
it is chosen. The axes span every group's series, so choosing another does not move them, and the
legend lists the chosen group's alone.

```
 "groups":[{"label":"101X+","series":[0,1],
            "readouts":[{"label":"RMS error","text":"0.4 dB","status":"pass"}]},
           {"label":"102X+","series":[2,3]}]
```

`y_min` (optional) makes a series an envelope: the service decimated it to the plot's width,
and each point stands for an interval of the original lines, `y` the largest value in it and
`y_min` the smallest. The shell draws the band between them (and with `steps`, each interval
flat across its `edges`), so no peak or notch is lost to the decimation.

This is the drawing rule (`plot.drawing_shape`, `plot.bin_edges`); the analysis
service takes it from there rather than restating it.

**`reading`**

```
{"view":"reading","title":"...",
 "rows":[{"label":"...","value":<number|string>,"unit":"..."|null,"status":"pass"|"warn"|"fail"|"info"}]}
```

`status` is shown as a word or symbol as well as a color.

**`shape3d`**

```
{"view":"shape3d","title":"...","unit":"m",
 "nodes":{"$array":.. f32 [N,3]},
 "lines":{"$array":.. u32 [L,2]},          optional
 "triangles":{"$array":.. u32 [T,3]},      optional
 "modes":[{"index":1,"frequency_hz":12.3,"damping":0.01|null,"shape":{"$array":.. f32 [N,3]},
            "shape_imag":{"$array":.. f32 [N,3]},   optional
            "label":"Operating shape"}]}             optional
```

A shape is one translation vector per node, in the same node order as `nodes`. A complex shape (a
complex mode, or an operating deflection shape read from FRFs at one line) carries its real part in
`shape` and its imaginary part in `shape_imag`; a real shape has no `shape_imag`. The shell animates
`Re((shape + i shape_imag) exp(i theta))` over a cycle, so a real shape moves as a standing wave and
a complex one shows its traveling part. `label`, when present, names the shape in place of
"Mode <index>", for a shape that is not a mode.

`overlays` and `overlay_phase` (optional) carry a second set of shapes on the same nodes, for a
`matrix` view that `selects` this one (below).

`pick` (optional) says what a click in the view does. A click is a press and release in one place; a drag still turns the
view. The shell runs `op` with `params` and what was clicked, and the views it answers with take
the result's place:

```
"pick":{"op":"project.add_node","params":{"geometry":"Plate"},
        "target":"point"|"nodes"|"faces",
        "hint":"Click in the view to place a node",
        "ids":[1,2,3,...],             the node id of each of the model's own rows, first rows first
        "count":3,                     nodes: how many make one; 0 takes any number, then Enter
        "param":"nodes",               nodes: the param the ids go in, as "4, 7, 9"
        "faces":{"$array":.. i32 [T]}, faces: each triangle's surface, -1 for none
        "kept":[1,3],                  faces: the surfaces in use now
        "out":"leave_out","back":"put_back",   faces: the params a click names a surface in
        "off":2,                       nodes: marked places, shown through the model; this many of
                                       the last `ids` are left out and drawn dim
        "others":[40,6]}               nodes: a second set of places in the rows after `ids`, drawn
                                       in their own color and not clicked: this many held, then
                                       this many not (a mate's other side)
```

A `shape3d` with `opacity` (below 1) opens with its surface that see-through, so places marked on
surfaces inside the model show; the view's own slider changes it after.

A `point` click sends `x`, `y` and `z` in the view's unit: where it lands on the model's surface,
on the model's plane when the model is flat, or else at the depth of the node nearest it. A
`faces` click names a surface in `kept` in the `out` param and any other in the `back` param,
taking it out of the other; the shell draws the surfaces not in `kept` muted.

A `shape3d` may instead play measured data back on the mesh: `modes` is empty and `motion` says
how each measured DOF moves, sample by sample.

```
"motion":{"label":"Time","unit":"s","start":0,"step":0.000244,   the abscissa: start + k step
          "quantity":"Acceleration","value_unit":"m/s²",
          "node":{"$array":.. u32 [D]},          the node of `nodes` each DOF moves
          "direction":{"$array":.. f32 [D,3]},   the unit direction it moves in
          "values":{"$array":.. f32 [D,S]},      DOF d at sample k
          "peak":130.4}                          the furthest any node gets, in value_unit
```

With `modes` (f32 [M,D,3]), `values` is instead [M,S], modal coordinates: `node` lists the D nodes
that move, `direction` is empty, and sample k's offset at node d is the sum over modes of
`modes[m, d] * values[m, k]`, so the physical response never has to be formed or sent.

At sample k each node is offset by the sum over its DOFs of `direction * values[d, k]`, scaled
so that `peak` is the amplitude the person chose. Sending each DOF's samples rather than a frame
of every node keeps a large mesh with a few sensors as small as the sensors' data; nodes with no
DOF stay where they are. Several DOFs may move one node (X, Y and Z, say). The shell plays a
record through in about 10 s at normal speed, from an eighth to eight times that, and colors each node by how far it is from rest at that sample.

`link` (optional) ties playback to a `spectrum` of the same result with the same `link`: the
spectrum marks the time the mesh shows with a line, and dragging that line moves playback there.
The tie holds within one result; the hover cursor `link` names is shared as for any spectrum.

**`waterfall`** (many records of one quantity, each at its own depth)

```
{"view":"waterfall","title":"...",
 "x":{"label":"Frequency","unit":"Hz","scale":"linear"|"log"},
 "z":{"label":"PSD","unit":"(m/s²)²/Hz","scale":"log"|"linear"},
 "records":["101Z+","102Z+",...],           one label per record, front to back
 "offsets":{"$array":.. u32 [R+1]},         record r is points offsets[r] .. offsets[r+1]
 "x_values":{"$array":.. f32 [P]},
 "z_values":{"$array":.. f32 [P]},          NaN is a gap: the line breaks there
 "x_range":[low, high],                     where something is drawn
 "z_range":[low, high],                     optional: a level range the data pins
 "page":{"number":1,"count":3,"records":905},   optional: which page of how many
 "quantities":[{"value":"acceleration|","label":"Acceleration","records":220},
               {"value":"force|","label":"Force","records":40}],   optional: two or more
 "quantity":"acceleration|"}                optional: the one drawn
```

Values are in display units and never logged: the shell takes log10 of an axis whose `scale` is
`log`, so a value at or below zero is a gap there. The service has already drawn each record as it
is calculated, the same rule as `spectrum`'s `shape`: a time history arrives peak-decimated to a
few thousand points that keep every extreme, a PSD as its step outline (two points per bin, never
thinned, so the area under it is its mean square), and a breakpoint specification filled in along
its power law. The shell colors every point by its level on the sequential colormap, with a
numeric legend, and names at most 40 records, every nth past that. One quantity is drawn on a
stage, since one level axis cannot hold accelerations and newtons at once; an object holding more
than one lists them in `quantities`, and choosing another reruns the operation with its `quantity`
param set to that `value`, starting again at page 1. Hovering a curve reads its record, abscissa
and level. Records that do not fit one
scene are on later pages, never drawn coarser. This is the 3-D data view
(`hertzforge.drawing.viz.waterfall`).

A waterfall can also be one of the stages, with these optional fields:

```
 "stations":["101Z+","102Z+",...],          the depth axis's labels, front to back
 "depths":[0,0,1,1,...],                    each record's station: two records can share one
 "roles":["reference","measured",...],      each record's pen; "measured" is colored by level,
                                            any other role is drawn in that role's color
 "zones":[{"station":0,"kind":"warning"|"abort","side":"upper"|"lower",
           "x":{"$array"..},"inner":{"$array"..},"outer":{"$array"..}}],
                                            a limit band; no outer runs it to the top or floor
 "exceed":[{"station":0,"side":"upper"|"lower","x0":..,"x1":..,"z":..}],
                                            lines past an abort limit: one rectangle each, from
                                            its limit's level to the top (upper) or floor (lower)
 "walls":[{"$array"..}],                    one abscissa a station: a translucent wall
 "surface":true                             the records are one surface, each on the same
                                            abscissa, rather than separate lines
 "regions":[region...]                      spans of the abscissa across the whole stage, as a
                                            spectrum's regions are, dragged the same way
 "note":"..."                               optional words under the toolbar
```

With `stations` the depth axis is the stations, not the records; `records` still names each
curve for hovering.

A waterfall may instead be a stage of paths: with `y` (an axis) and `y_values` (f32 [P], one a
point) every point stands at a depth of its own on that axis, `y_range` its span, rather than at
its record's station. This is the sine stage: frequency across, time receding,
amplitude up, each tone's requirement and limits a path through time and each extracted level its
own path placed by its clock. A `zones` entry then carries `y` beside `x` and an `exceed` entry
`y0` and `y1` beside `x0` and `x1`, so the bands and the rectangles follow the path; the depth axis
is ticked in `y`'s unit and each record is named at the start of its path. The paired stage overlays two densities at each channel they share, the
banded stage draws a specification's bands with or without a measurement, and a scalogram is a
surface with its cone of influence as two walls.

**`matrix`** (one value per row and column, such as a MAC)

```
{"view":"matrix","title":"...",
 "rows":{"label":"Identified","ticks":["439.2 Hz","647.2 Hz",...]},     one tick per row
 "columns":{"label":"FE model","ticks":["439.2 Hz",...]},              one tick per column
 "values":{"$array":.. f32 [R,C]},          row r, column c
 "value_label":"MAC",
 "value_range":[0, 1]}                      the color scale, pinned
```

The shell draws one cell per value, colored on the sequential colormap over `value_range` with a
numeric legend, the first row at the top and the first column at the left, and writes each value
in its cell when the cells are large enough to hold it. Hovering a cell reads its row, column and
value. A pinned range makes two matrices of the same quantity read alike.

A matrix that compares two sets of shapes (a MAC) may come with the `shape3d` view of those shapes
in the same result, and then says `"selects":"shape3d"`. That view's `modes` are the rows' shapes
and its `overlays` the columns', and `overlay_phase` (f32 [R,C]) is the angle that aligns column
c's shape with row r's, since a real mode is defined only up to its sign and a complex one up to a
rotation. Clicking cell (r, c) shows mode r with overlay c drawn over it, rotated by that angle and
in one flat color, scaled to the same amplitude; the playback and the camera carry on through a
click.

**`heatmap`** (rows of values against frequency, such as MIMO Random's error per control)

```
{"view":"heatmap","title":"Control Error",
 "rows":{"label":"Control","ticks":["Control X",...]},         one tick per row
 "x":{"label":"Frequency","unit":"Hz","scale":"log"},
 "x_values":{"$array":.. f64 [C]},          the frequency of each column
 "values":{"$array":.. f32 [R,C]},          row r, column c
 "value":{"label":"Error","unit":"dB"},
 "row_bands":[{"alarm_db":[3,3],"abort_db":[6,6]}, ...]}         one per row
```

The color breaks are each row's own alarm and abort bands. The scale is lightness-ordered so the
bands read without hue, and hovering a cell reads its row, frequency and value. The service sends
one column per pixel of the plot, each the line nearest the column's center.

**`sdm`** (a block of a spectral density matrix, such as MIMO Random's matrix grid)

```
{"view":"sdm","title":"Spectral Matrix","channels":["Control X","Control Y"],
 "packing":"upper","unit":"g²/Hz",
 "x":{"label":"Frequency","unit":"Hz","scale":"log"},"x_values":{"$array":.. f64 [L]},
 "measured":{"re":{"$array":.. f64 [L,P]},"im":{"$array":.. f64 [L,P]}},
 "reference":{"re":..,"im":..}}
```

`P = n (n + 1) / 2` for the `n` channels: each line's upper triangle, row by row (see "MIMO
Random"). The diagonal is each channel's PSD; an element above it gives the coherence
`|G_ij|² / (G_ii G_jj)` and the phase of `G_ij`, under the `E[X X^H]` convention. `measured` is
null until the run has measured; `reference` is null when there is none.
**`table`** (rows of text under column headings, such as a mode table)

```
{"view":"table","title":"...",
 "columns":["Mode","Frequency [Hz]","Damping [%]","Description"],
 "rows":[["1","439.2000","1.250",""],...]}          one cell per column, as text
```

Every cell is text, written the way the value is usually quoted (the service formats it). The
shell draws the table with its headings held at the top as it scrolls, puts a cell that reads as
a number at the right of its column, and copies the whole table as tab-separated text, the
headings first, so it pastes into a spreadsheet.

**`bars`** (a bar a row, such as each channel's RMS error against its tolerance)

```
{"view":"bars","title":"RMS Error",
 "value":{"label":"RMS error","unit":"dB"},     unit null for a pure number
 "labels":["101X+","102X+",...],                one a row, top to bottom
 "values":[-4.0,0.5,null,...],                  null: no number, so no bar
 "states":["under",null,null,...],              each bar judged: "over", "under" or null (in)
 "notes":[null,null,"not scored: ...",...],     optional: said along the row
 "under":-3.0,"over":3.0,                       optional thresholds, null or absent for none
 "baseline":0,                                  optional: where the bars grow from
 "floor":0}                                     optional: the axis reaches no lower
```

The service judges every bar and the shell draws what it is told: a bar whose state is set is in
the fail color with its state written beside the row, and the ground past each threshold is
shaded. A row with no number keeps its name and its note. Bars grow from `baseline` (0 unless
given), which is a kurtosis's Gaussian 3 rather than a zero no record sits at. Rows are named down
the side and the chart is as tall as its rows, so sixty channels stay legible.

**`image`** (a photo of the test)

```
{"view":"image","title":"setup","format":"png"|"jpeg","bytes":{"$array":.. u8 [N]}}
```

The file's bytes as they were read, never re-encoded, so the report and the view show the same
picture; the shell draws it at its own aspect under its name. A Photos object selected in the tree
shows one `image` a photo, the picked ones when some are picked, stacked.

A `shape3d` built from a geometry carries `node_ids`, the drawn nodes' ids (u32, one per node), so
the view can caption the node under the pointer, coincident nodes listed together.

A table may carry `"edit":{"op","params","columns","choices"?}`: its cells are typed into in place. `columns`
gives each column's key, or null for a column that is read only. A cell typed into runs `op` (an
operation, by `op.run`) with `params` and `row` (from 1), `column` (the key) and `value` (the text),
and the first `table` view of the answer takes the table's place; a value the operation refuses
leaves the cell as it was and the refusal is shown. Tab-separated text pasted into a cell sets
the block it covers, cell by cell. `rows`, when the table shows some of the object's rows, gives the
row each shown row is, from 1. `remove` (`{"op","params","column"}`) says the rows are the entities
themselves, as in a geometry's entity tables: Delete removes the selected rows rather than clearing
their cells, by running `op` with `params` and `ids`, the selected rows' cells in `column` (their
ids) as runs (`"1-3, 7"`). A table longer than a thousand rows sends its first thousand with `total`,
its count, and `more` (`{"method","params"}`); the shell asks `method` with `params` and `start`
and `count` (at most 5000) for the rows it scrolls to, in the display units as the table is. An
entity table of a part says the part in its edit's and `more`'s params (`family` or `groups`), and
`project.set_entity_cell` takes a row as that part's.

With a part of a geometry picked (`part`), `project.show` draws its entities' table beside the
geometry, with the click in the view that adds one: the nodes or the coordinate systems, a family's
elements, or picked element groups' table and their elements. The Solids row or a solid picked draws
each part's surface, meshed or not, beside the solids table.

## Operations: Declarative Panels

The analysis service describes each operation; the shell builds the panel from the description.
A feature that fits this pattern needs no TypeScript.

```
{"id":"project.fit_modes","title":"Fit Modes","workspace":"modal","summary":"one sentence",
 "params":[{"name":"frf","label":"FRF","type":"enum","options_from":"verb:fit_modes"},
           {"name":"low_hz","label":"From","type":"number","unit":"Hz","default":0,"min":0},
           {"name":"high_hz","label":"To","type":"number","unit":"Hz","default":1000,"min":0},
           {"name":"count","label":"Modes, at most","type":"integer","default":10,"min":1,"max":60}],
 "outputs":["shape3d","reading"]}
```

Param `type`: `number`, `integer`, `enum`, `boolean`, `string`.

An `enum` that names one of the project's objects sets `"options_from":"objects:<kind>"`, or
`"objects:<kind>|<kind>"` for more than one kind; the shell fills it with the names of the open
project's objects of those kinds, in the project's order. Left out, it takes the first of them.
An operation run when the project has none of the kinds a required param takes is refused, and
the refusal names the kind.

An `objects:` or `verb:` enum whose `default` is `""` is optional: `""` names no object, the shell
offers it first as "(none)", and the tool is shown even when the project has no such object.

An `enum` that names an object a core verb takes sets `"options_from":"verb:<verb>"`, such as
`verb:integrate`; the shell fills it with the objects whose `verbs` (in the project summary) list
that verb. Which verb applies to what is the core's own table (`Project.verbs`): Integrate takes a
time history whose channels are accelerations or velocities, Compute FRFs one whose channels have
both a response and a reference side, and so on, which no list of kinds can say. One more is
listed beside the core's: `overload_check`, for a time history a linked channel table describes,
which the Overload reading reads against that table's ranges. Left out, it
takes the first of them, and an operation run when no object takes the verb is refused.

A param with `"setting":"<group>.<field>"` opens on that setting of the object the operation's
`source` param names, from the object's `settings` in the project summary, such as
`averaging.frame_length`; when the object has no such setting, it opens on its default. The shell
fills it again each time the source changes, so the form shows what the object holds before it is
changed.

A param with `"length":true` is a length, or a text of lengths (`0, 0, 0.1`, `3: 0.01`), typed in the
connection's display system: the service takes it to meters before the operation runs, and passes it
as it is on a geometry whose units are not defined. The descriptor says `m`; the shell shows the
system's length unit for its `unit` and for `[m]` in its label, none on an undefined geometry, and
when the system changes it converts what the form holds, which changes nothing in the model.

A param with `"suggest"` names a request the panel asks, with the form's values, when the button
beside the field is pressed; the `value` it answers with fills the field (Mesh's element size,
`project.mesh_size`). Mesh is offered on the solids picked, filled with them, with shells for flat
parts and hexes otherwise, and with the size they are likely to hold at; its row's `omit` leaves
out the ids, a size per piece and detecting each piece's size.

A param with `"shown_with":[name, value]` is shown only while the param `name` holds `value`: of two
ways to say a thing, the panel shows the field of the one chosen (Solve Modes: `by` is `count`, with
`num_modes`, or `frequency`, with `maximum_frequency`).

A param with `"in_view":true` is not shown in the panel: the result's view sets it. The shell runs
the operation again with the panel's values and the view's choice, and shows the new result in
place of the old. A waterfall's `quantity` is one: the view lists the quantities it can draw and
the person picks one there.

`workspace` is the module the operation belongs to, the rail entry that offers it: `acquire`,
`simulate`, `signals` (recordings and what is read straight off them), `random`, `shock`, `sine`,
`transient`, `sysid`, `modal`, `geometry`, `specify` or `report`. The analysis is split by what is studied, so a sine
test is not offered shock tools. `geometry` holds the finite element work too (meshing, element
group properties, solving for modes), which was a `model` module of its own until 2026-11-10; the
shell shows it as Model. The modules share one project, one tree and one way of showing a
selection; they differ in which operations their action table offers. An operation's `modules`
lists the modules that offer it: its own `workspace`, or more when several studies need it (Export
in all of them, Averaging in Signals, Random and Modal).
Every operation runs on the connection's own project, and `op.run` is refused while no project
is open. Its result carries the project's summary beside the views, `{"views":[...],"project"}`,
and the project counts as modified when the operation added an object. The operations are the
core's verbs (Waterfall, Compute PSDs, Compute FRFs, Fit Modes, Mode Shapes and the rest); the
shell shows a tool only when the project holds something it runs on ("tools follow the data"). A
view drawn by a verb that adds an object offers no in-view choice, since choosing would run the
verb again. Until 2026-11-05 the service also offered a catalog of built-in datasets (the plate
runs and three finite element models) with operations of their own; those are now example
projects downloaded from hertzforge.com and opened like any other file.

## Methods

**Analysis service**

| Method | Params | Result |
|---|---|---|
| `catalog.list` | `{}` | `{"operations":[descriptor...]}` |
| `op.run` | `{"op","params":{...},"preview"?}` | `{"views":[view...],"project"}` |
| `project.new` | `{"title"?}` | `{"project"}` |
| `project.open` | `{"path"}` | `{"project"}` |
| `project.import` | `{"path","units"?}` | `{"added":[name...],"project"}` |
| `project.save` | `{"path"?,"compress"?}` | `{"path","project"}` |
| `project.drag_out` | `{"project":true}` or `{"names"}` | `{"paths"}` |
| `op.cancel` | `{}` | `{"op"}` |
| `report.*` | see Reports below | |
| `project.rename` | `{"name","to","index"?}` | `{"project"}` |
| `project.remove` | `{"names"}` | `{"project"}` |
| `project.delete_parts` | `{"parts":{name:[index,...]}}` | `{"project"}` |
| `project.activate` | `{"geometry"}` | `{"project"}` |
| `project.duplicate` | `{"names"}` | `{"project"}` |
| `project.refresh` | `{"names"}` | `{"project"}` |
| `project.units` | `{"name"}` | `{"name","tables":[{"side","rows":[...]}]}` |
| `project.children` | `{"name"}` | what the object holds (below) |
| `project.entity_rows` | `{"geometry","kind","family"?,"groups"?,"start","count"}` | `{"rows"}`: more of an entity table that sent its first rows (its `more`) |
| `project.mesh_size` | `{"geometry","solids","elements"}` (a Mesh form's values) | `{"value"}`: the element size those solids are likely to hold at, in the display units |
| `project.show` | `{"objects","records"?,"part"?,"prefs"?}` | `{"title","views","note","offers"}` |
| `project.actions` | `{"objects","records"?,"part"?}` | `{"table"}` |
| `project.journal` | `{}` | `{"lines"}` |
| `project.define_units` | `{"name","units":[{"side","key","unit"}]}` | `{"converted":[record...],"project"}` |
| `tables.save` | `{"kind","table_kind"?,"columns","rows","path","x_unit"?,"y_unit"?}` | `{"path"}` |
| `tables.load` | `{"path","kind","table_kind"?,"x_unit"?,"y_unit"?}` | `{"kind","table_kind","columns","rows","warnings","x_unit"?,"y_unit"?}` |
| `units.system` | `{"name"?}` | `{"name","systems","length":{"unit","to_si"},"quantities"}` |
| `fit.*` | see Fitting modes below | |

**Gizmos.** A `shape3d` with `gizmo` draws arrows and rings at `origin`, along the three `axes`.
Letting go of one runs `slide` or `turn`: its `op` with its `params`, the amount in meters or degrees
in the param `values` names for that axis, and what `adds` (optional) gives for that axis. `origin` is
in the view's `unit`; `to_si` (optional) is what a length dragged is multiplied by to send it in meters.

**Previews.** An operation with `previews` can be shown while its form is typed: `op.run` with
`"preview": true` runs it on a copy of its geometry and answers `{"views"}`, with no `project`.
Nothing reaches the project or its journal. A reading's `Added` row says `Would add`. `preview` on an
operation without `previews` is a `bad_request`, and nothing runs.

**Commits.** An operation with `commits` names a boolean param that says go ahead. Run with it
false, the operation answers with what it would do and changes nothing; run with it true, it does
it. The shell's form does not ask that param: its Preview button sends it false and Apply sends it
true.

**The console.** `project.journal` gives the session as Python: every verb, settings change and
import since the project was made or opened, as the line a script would make (`Project.journal`),
the first line making the project. The shell shows it read-only along the bottom of the window, to be copied into a script; nothing is typed into it.

**The tree.** Each object in the summary carries `holds`, a few words on what it contains, and
`banded` when a PSD or a specification is on proportional bands, which changes its icon.
`project.children` answers what one object opens into, by
`kind`: a `geometry` lists its `parts` (nodes and coordinate systems with counts), its element
`families` (every family, each with the element `groups` holding elements of it and their counts),
its `solids` (a boolean's operands as its `children`) and its `mates`; a data object is a `grid`,
`rows` of a DOF and a quantity (a time history's with a `role`), `columns` of what tells one record
from another (reference DOFs, capture labels, or one blank), `cells` of record indices (-1 for a
hole), one icon key per record in `icons` (a quantity, two as `response/reference`, or empty for
the object's own icon) and the records whose units are `undefined`; a shape set lists its `modes`;
a channel table its `channels`; photographs their names; matched modes their `matches`; anything
else is `none`. The answer describes the object as it stands and is asked again when the project
changes.

**Links and what fits.** The summary's `links` are the project's object groups,
bracketed down the tree: each group's `members`, its `role` (`Basis`, the group comparisons are
made against, or null) and its `name` (blank when it has none). An object that does not fit the
geometry it answers to (its group's, else the active one), or a modal object with no shape set to
answer to, carries `issue`: the `message` to show, and the `sub_items` (record, mode or channel
indices) at fault, for the grid to mark. Incompatibility warns and never blocks, so a linked object
with an issue is still linked. `project.link`, `project.unlink`, `project.set_basis`,
`project.place`, `project.set_role` and `project.name_object_group` are operations (run by `op.run`)
and answer with the project, as every act does.

**The selection shows.** `project.show` answers what a selection in the tree shows: `objects` are the selected objects' names, `records` maps an object's name
to the record indices picked in its grid (a mode's or a channel's index for a shape set or a channel
table), and `part` names a part of a geometry picked (`{"kind","id"}`: a family, an element group, a
solid, a mate; `ids` too for element groups picked together) or is null. One object shows by its kind: a geometry draws; a data object is its
waterfall, or the records picked as curves; a shape set is its modes on the geometry of its group (or
the active one) with its mode table, or the table and its MAC against itself without one; a channel
table is its table. Two together show what the pair means: data on a geometry animates it (a shape set,
a time history played back, an FRF's or a spectrum's operating deflection shape at its loudest line, a
PSD's envelope), a channel table on a geometry marks its channels, two shape sets give their MAC, a
measurement beside its requirement gives the comparison's spectra, two PSDs overlay. A selection with
no joint reading shows each object on its own, and `note` says what was left out. The views come in the
connection's display units, as an operation's do. `prefs` is how the window reads data, kept across
selections: `flat` (the plot in place of the stage for a whole data
object), `log_frequency` (decades on every frequency axis) and `component` (`magnitude`, `real`, `imag`
or `phase` of complex data) and `line`, the frequency in Hz the stage's operating shape or envelope is
read at (null for the loudest line); `offers` says which of them the selection can take (`flat` for a
data object, `complex` for complex frequency data, `frequency` for anything on a frequency axis). A
`shape3d` that reads at a line carries `cursor: "frequency"`, and its plot carries `link: "frequency"`,
so the plot's cursor moves the line. A `waterfall` may say its opening `facing` (a coherence opens on
the map, `top`). Several data objects of one type show on one stage, merged for the drawing; a CPSD
on a geometry shows its principal shape, or the shape relative to one reference when one column of its
grid is picked. Two shape sets (with or without a geometry in the selection) show their MAC and, on a
geometry, the stage the matrix `selects` a pair on; a `matrix` may carry `matched`, the `[row, column]`
pairs already kept in a Matched Modes object for the two sets. `prefs.projection` (offered as
`projection` when the two sets are on different geometries) animates the second set projected onto the
first's geometry; `project.add_matches` keeps picked squares as matched modes. A lone shape set shows
one view at a time, named by `prefs.shape_view` (`offers.shapes`): `animate` (its modes on its geometry,
and the default), `table`, `mac` (with the stage its squares animate) or `values`, the
values read at `prefs.values_dofs` (`all` or a channel choice of the table in its group, listed in
`offers.values_dofs`) in `prefs.values_form` (`shapes`, `response`, `reference`). An FRF beside a shape
set shows the picked records with their resynthesis following dashed (`follows`). `prefs.arrows` and
`prefs.sensors` (offered as `arrows` and `sensors`) put DOF arrows and the channel table's sensors over
every stage. The arrows are `marks` on the `shape3d`: static line work, `nodes` f32 [M, 3] in the
geometry's coordinates and `lines` u32 [L, 2] indexing them. The sensors are `bodies`: solids drawn
from each model's catalog shape and size, `nodes` f32 [P, 3] in the geometry's coordinates,
`triangles` u32 [T, 3], `colors` u8 [P, 3] (sRGB) and `follow` u32 [P], the row of the view's
`nodes` whose motion each point takes, so a body moves with its node (2^32 − 1 stays put).
`project.sensors` draws its view's sensors the same way. A lone channel table shows typed into, its cells
edited with the widgets `edit.kinds` names (a flag by a check box, a choice from its list, a date in a
date field, a unit from a shortlist); `prefs.environment` picks the loop whose control channels the
Control column shows (`offers.environments`) and `prefs.table_view` the table or one of that loop's
transformations (`offers.table_views`). `project.save_as` takes `units`: `display` for the connection's
display unit system (the server puts its name in), `si` for the stored values.

**Export's formats.** The Export row carries `formats`, each `{"value","label","suffix"}`: what that
object can be written in, `.hforge` first. The desktop app lists them as the save window's file types.

**The actions a selection takes.** `project.actions`, with the same `objects` and `records`, answers
for the action table beside the project tree, as `{"table":{"columns":[...]}}`. An operation is offered
when the selected objects fill its object params: each object on a param that takes its kind (or whose
verb applies to it), every object used and every required object param filled. With nothing selected, the first column is the project: the acts that take no object
and the project's files. `fill` is the params to set from the selection (and `records`, as `'1-3, 7'`,
when records are picked and the operation takes them).

The first column has `"current": true` and is the selection; each other column is the selection with one
more object of the last type in its `kinds`, `"*"` meaning any object. A row is
`{"id","verb","kind","ready","reason"?,"modules"?,"fill"?,"candidates"?,"part"?}`. `kind` is `default`
for what the selection draws by itself (`id` starting `show.`), `run` for an operation (`id` is the
operation's) or another of a lone shape set's views (`id` starting `show.`), and `self` for an act on the selection that is not an operation (`id` starting
`self.`). A row that cannot run has `ready` false and a `reason` to show, including one whose operation
would refuse what the selection holds, as far as that can be told with nothing typed. A row without `modules` is
offered in every module. In a combination column, `candidates` names the project's objects that
complete the row, and a row with none is not ready. `part`, the part of a geometry picked in the tree
as `project.show` takes it, offers the operations on that part alone, each carrying `"part": true`,
and none of the geometry's own: the shapes to build from on the Solids row, Add Element and
Add Group on an element family (the family filled in), and on element groups what sets their
properties, merges, aligns or mates them, with every group picked filled into `groups`, and on
solids what edits, moves, patterns, takes apart, meshes or deletes them, with every solid picked filled into
`solids`. Element groups or solids picked together come as `{"kind": "group", "id", "ids"}` (or
`"solid"`), `ids` every one picked. With a part picked, the first column has `parts`, the part's kind
once for each one picked, and the only other column is that part with one more of its kind (`parts`
one longer): what two or more picked together offer (merging, aligning and mating element groups;
combining solids), its `candidates` the ids of the geometry's other groups or standing solids. Picked
together, those rows move to the first column. A row that acts on one part (Edit Solid) is not ready
with several picked. A shape's Add on the Solids row has `"instant": true`: the shell runs it at once
with its defaults and picks the new solid. New Geometry on the project row is instant too: it adds a
geometry named Geometry (then Geometry (2) and on) in the display system's length unit, unless its
`unit` names another, and the shell picks it; so is New Channel Table, named Channel Table. One solid picked offers its shape's Add as `Edit`, with
`"live": true` and `fill` holding the solid's own values (lengths in the display units) and its id in
`solid`; run with `solid` set, an Add changes that solid instead of adding one, only what differs from
it, and a live form is run each time a changed field is left, with no Apply button. Edit Solid stays for
booleans and patterns. A solid added, edited or picked is drawn in the view's `bodies`, in its own
color, with the gimbal (`gizmo`) on it. A row's `omit` names params its form leaves out because they
do not apply to what is picked: Set Properties on element groups with no shells is not asked for a
plate thickness, and no row on a part picked asks again for what the pick fills (its groups,
solids or mates: more are picked in the tree). Mate Surfaces is offered on two element groups
picked together, as Mate, and on one mate picked, as `Edit`, with `fill` holding the mate's name
and how it holds (stiffnesses in the display units); it leaves out `mate` on groups and `groups`,
`tolerance` and `patch` on a mate.

**Display units.** Every view the analysis service sends is drawn in its connection's unit
system. A connection starts
in the default (`in-slinch-lbf-s (g)`: inches, pound-force, accelerations in g);
`units.system` with `name` (one of `systems`) changes it, and without one says which it is, and
`length` gives its length unit and what takes that unit to meters. A param with `quantity` (a
dimension, `force/length` for a stiffness) is typed in the system too: `quantities` gives, by
dimension, the unit it is typed in and what takes that unit to SI, and the service takes it to SI
before the operation runs. Each
operation computes in SI; the service converts what it sends: a `spectrum`'s axes, series, step
edges and regions, a `reading`'s rows, a `waterfall`'s axes and values, the fit's CMIF, and the
report's page, and a `table`'s columns headed "<name> [<SI unit>]" (each number in them). A column
typed into (`edit`) is converted too when each of its cells is one number, and `edit.to_si` gives,
by column key, the [scale, offset] the shell takes a value typed back to SI by before it sends it;
a column of several numbers to a cell stays in SI. A unit it does
not recognize and a `matrix` are sent as they are. A `shape3d`'s coordinates are put in the system's length unit (its `unit` says which) and a measured motion's values in theirs; its shapes stay as they are, and a `point` pick carries `to_si` [scale, offset], what each coordinate clicked is taken back to SI by (SI = value × scale + offset) before it is sent; a `gizmo`'s `origin` is converted too, and its `to_si` takes a length dragged back. A view already on screen is not converted again; the shell draws it again.

**Projects.** Each connection has one project of its own, and it ends with the connection. A
connection starts with an empty project titled "Project", so every operation has one to work on. A
file reaches the service by its path on the service's machine; nothing is uploaded. `project.open`
replaces the project with the one a `.hforge`, `.vdyn`, `.mat` or ESCDF `.h5` (`.hdf5`, `.escdf`)
file holds, or with a new project holding the object when the file holds one, as Save As writes
them, and refuses any other file with a pointer to import it. A control run's folder (or the
`run.json` in it, `docs/hforge-format.md`) opens as the project file it converts to,
`<run name>.hforge` inside the folder, converted first when there is none or the run has changed
since. A client asks before it opens over a
project whose `modified` is true. `project.import` adds a file's
objects to the project, a run folder's as its converted file's. `units` declares what the file does not state, as the core's `import_file` options
(`length_unit`, `ordinate_unit` and the rest). `project.save` writes to `path`, or to the file the
project came from; a `.vdyn` is opened but never written. `project.drag_out` writes the whole project (`"project": true`) or the objects `names` to files in a new temporary folder, for the desktop shell to drag out of the window, and answers their paths: the project as one `.hforge` named for its title, each object as a `.hforge` object named for itself. It is a copy: the project's `file` and `modified` are unchanged. `project.new` replaces the project with an
empty one; it takes an optional `title` and defaults it to "Project". `project.rename` gives the object `name` the name `to`, and every
group, provenance record and report block that names it follows; a name in use is refused.
With `index`, `project.rename` renames one photo of a Photos object instead (the photo at that
index, from 0), and the report blocks that show it follow. `project.remove` takes `names`, a list,
and removes them all or, when one is not in the project, none. `project.delete_parts` deletes
sub-items picked in the tree, `parts` mapping an object's name to the indices picked: records of a
data object, modes of a shape set, channels of a channel table, photos of a Photos object. Taking every item is refused ("delete the object
instead"), and the journal records the call a script would make. `project.activate` makes `geometry` the active geometry, the one data is drawn on when
nothing else names one. `project.duplicate` adds a copy of each object in `names` beside it, named
"<name> copy", in no group. `project.refresh` computes each derived object in `names` again from
its source, in place and under its own name; an empty list refreshes every stale object, sources
first. Each answers with the project. A `.png`, `.jpg` or `.jpeg` path is a photo: it is added to the project's Photos object,
made on first use, and `added` names that object.

Units a file did not state are declared by channel, not by record. `project.units` gives an object's tables: a geometry's one row (`side`
`object`, key `length`), a shape set's (key `mass`), and a data object's channels, one row per
(DOF, quantity), since a drive point's accelerometer and load cell share a DOF and are two
channels. An FRF has a `response` and a `reference` table; a cross-spectral matrix and every other
data object one `channels` table. Each row has `key`, `label` (the DOF), `quantity` (what the
channel measures, from its dimension or the file's claim, or null), `unit` (null while undeclared)
and `choices`, the units offered first; any unit Hertzforge reads is accepted. `project.define_units`
declares units by `side` and `key`. Every unit is read before anything changes. A record converts
when each of its channels is named, here or before, and one only half named waits; `converted`
lists the records converted. Project requests on one connection run one
at a time, in the order they arrive.

The project as the shell lists it:

```
{"title":"Bracket Survey","file":"/Users/.../survey.hforge"|null,"modified":true,
 "objects":[{"name":"FRF","kind":"frf","records":22,"source":"SIM",
             "units_defined":true,"groups":["Group 1"],"verbs":["fit_modes"]},
            {"name":"Geometry","kind":"geometry","units_defined":false,"groups":[],
             "verbs":["generate_rigid_body_modes","add_solid_block"]}]}
```

`kind` is the object's class name in lower case with underscores (`time_history`, `psd`, `frf`,
`multiple_coherence`, `shape_set`, `geometry`, `channel_table`, `specification` and so on).
`records` is present for data objects, `source` for measured data, and `units_defined` is false
for an object imported without its units, which the shell marks. `groups`
names the object groups it belongs to, by role (`Basis`), name, or `Group <n>` in the project's
order. `active_geometry` is the active geometry's name, or null. `unresolved`, present only when
there are any, lists the objects the project holds by reference to another `.hforge` file that
was `not found` or has `changed` since: each with its `name`, `kind` and the `path` it was looked
for at. They are not loaded and no tool lists them; saving keeps their references, and
`project.remove` drops one. `stale` (present only when it is) says why a derived object no longer
matches its source: the source's settings changed after it was computed. `verbs` are the core's
processing verbs that apply to the object, in the core's order, then `overload_check` where a
linked channel table describes a time history; `verb:` params are filled from them. `settings` is
a time history's analysis settings, each group
present only when set: `averaging` (`frame_length` in samples, `overlap` as a fraction, `window`,
`frames`, `start` in seconds, `detrend`), `filtering` (`kind`, `low` and `high` in Hz as the kind
has them, `order`) and `truncation` (`start`, `stop` in seconds). A geometry's are `rigid`: the
reference point `x`, `y`, `z` in meters, `normalize`, and with it `mass` and the inertia terms
`ixx`, `iyy`, `izz`, `ixy`, `ixz`, `iyz`. A report's `settings` are
`features` (each report feature's key, true where it is on) and `identity` (each identification
field as text, people authorized comma-separated). `modified` is true after an import or an operation that added,
removed or changed an object, and false after an open or a save.

An operation reports its progress while it runs, as event topic `op.progress`,
`{"op","done","total"}`: a solve, a mesh, a signal generated, a sine extracted. `total` is -1 for a
stage that cannot count itself, such as one long call into a library, which the shell shows as a
moving bar. They come at most ten a second, always when the stage or its total changes, and always
the last one. `op.cancel` (`{}`, answering `{"op"}`, the operation running or null) stops the
operation at its next report: it is refused, saying it was cancelled, and the project is as it was.
A stage that cannot count itself is not interrupted; the cancel takes effect when it returns.

A reader that can count reports its progress while an import runs, as event topic
`project.progress`, `{"done","total","path"}`, at most ten a second and always the last one. A
file the core cannot read is refused in the core's own words (`refused`, or `not_found` for a path
with no file).

**Table files.** A channel table or a breakpoint table saved to a file and opened from one, by
path, as projects are; neither request touches the connection's project. `kind` is `channels` (the
channel table: one table that holds what the analysis reads and, converted for a test, the
settings the control service runs each channel at) or `breakpoints` (a profile, target or limit table, which
`table_kind` names: `random_profile`, `sine_profile`, `sine_limit`, `srs_target`, `limit` or
`drive_gain`; or Mixed Mode's parts tables, `mixed_tones` and `mixed_bands`, one row per part,
whose `id`, `name`, `follows` and a tone's `profile` (its rows as `freq quantity value`, separated
by `;`) are text). `columns` names the
table's columns and each row holds one value per column.

A channel table's columns are the channel table object's own (`channel`, `node`, `direction`,
`role`, `control`, `channel_type`, `unit`, `sensitivity`, the two ranges, the sensor's identity,
`comment`), then the controller's settings, an optional group present on a table converted to
carry them: `enabled`, `drive`, `physical`, `iepe`, `iepe_bias`, `polarity`, `range_max`,
`control_group`, `abort_accel_g`, `abort_velocity_ips` and `abort_displacement_mil`. A value is a whole number for `channel`, a whole number or `null` for
`node` and `control_group`, a number or `null` for the measurements (`sensitivity`, the ranges,
the bias, the abort limits), `true` or `false` for a flag, and text (`""` for blank) for the rest.
A control channel row maps onto them so: its `index` is `channel`, `description` is `comment`,
`units` is `unit`, `sensitivity_mv_per_eu` is `sensitivity`, `physical` is
`"<device>/<channel>"` (`""` when unbound), `interface` is `iepe` (true for IEPE), `iepe_bias_v`
is `iepe_bias`, and the abort limits are the three abort columns. Its role is read from three
columns: `drive` true is a drive, else `control` true is a control, else a `role` of
`reference` is a reference, and any other is a monitor. `tables.load` answers with every column
of the channel table, the controller's included: a table without them is converted on the way in
(every channel on, none a drive, each full scale from its clip level and sensitivity where both
are stated), and `warnings` says so. A client that edits only some columns keeps the others'
values for each row and sends them back with its own, so a table opened and saved keeps every
column and row. A channel table's control loops (its environments, which a `.hforge` table can
hold) are not among them: `tables.load` does not return them and `tables.save` does not write
them, so a `.hforge` table that held them is saved without them. A table saved with the controller's columns among its columns is written converted;
one saved without them is written as the measurement's table alone.

A breakpoint table's columns are its kind's: `freq_hz` and `psd_g2_hz`, `slope_db_oct` for a
Random profile; `freq_hz`, `quantity` and `value` for a Sine profile; `freq_hz`, `quantity` and
`bound` for a Sine limit channel's bound; `freq_hz` and `accel_g` for an SRS target; `freq_hz` and
`ceiling` for a limit; `freq_hz` and `gain` for the gain from the drive to a limit channel (g/V);
and for a profile the bands, `alarm_below_db`, `alarm_above_db`, `abort_below_db` and
`abort_above_db`, `null` where a row has none. A Sine table's `quantity` is `accel_g`,
`velocity_ips` or `disp_pp_in`, and each row's value is in its quantity's own unit, so its
`y_unit` is `""`. `x_unit` and `y_unit` are its units, its kind's own when not given.

`tables.save` writes the table to `path` in the form its suffix names: `.hforge` (a single-object
file, `docs/hforge-format.md`), `.csv`, `.tsv` or `.txt` (tab-delimited) or `.xlsx`; a path with no
suffix is given `.hforge`. A write is strict: a column that is not the kind's, a value the column
cannot hold (a sensitivity of 0, a polarity that is not `positive` or `negative`, a channel number
used twice) or a row of the wrong length is refused `bad_request`, naming the row and column, and
nothing is written. The file is replaced in one step, so a failed save leaves it as it was. A
channel table is written as the channel table object writes itself: `.hforge` as its
`channel_table/` object and `.xlsx` as its workbook, and the text forms under the same headings.
The text and workbook forms of a breakpoint table start with comment lines saying which table they
hold (`# Hertzforge table: breakpoint_table`, its `table_kind`, `x_unit` and `y_unit`).

`tables.load` reads the file at `path`. A channel table is read as the channel table reads any
source: a heading is matched to a column by name or alias, with a unit in it ignored
(`Sensitivity (mV/g)`), a column the table does not have is left out, and a value it cannot read
takes the column's default; each is named in `warnings`. A breakpoint table is read leniently too:
headings by name or alias, in any case and spacing and with a unit in them, comment lines (`#`,
`//`) and blank rows skipped, and a column that is not the table's left out and named in
`warnings`; only the columns found come back, in the table's order, and a cell it cannot read is
refused, naming its row and column. A file that holds another kind of table, or another
`table_kind`, is refused (`refused`) in words. Without `table_kind` a breakpoint file's own is
taken. The units that come back are the ones the file says (in its comment lines, its `.hforge`
attributes or a heading's unit, `Frequency (Hz)`), else the request's `x_unit` and `y_unit` (the
units of the table the file is opened into), else the kind's own; a client compares them with
its own and converts or refuses. A channel table has no units, so `x_unit` or `y_unit` on one is
`bad_request`. A missing file is `not_found`. A channel table file opens as a project holding the table,
as any single-object file does; opening or importing a breakpoint table file as a project is
refused with a message saying it is opened from the table it belongs to.

**Fitting modes.** One mode at a time, on the residual; the session is the core's `ModalFitSession`. Each connection has at most one fit, on its
project, and opening or starting another project ends it.

| Method | Params | Result |
|---|---|---|
| `fit.start` | `{"frf","shapes"?,"coherence"?,"records"?}` | `{"fit"}` |
| `fit.find` | `{"low"?,"high"?}` | `{"fit"}` |
| `fit.move` | `{"frequency"}` | `{"fit"}` |
| `fit.damping` | `{"damping"}` | `{"fit"}` |
| `fit.fit` | `{}` | `{"fit"}` |
| `fit.confirm` | `{"description"?}` | `{"fit"}` |
| `fit.refine` | `{}` | `{"fit"}` |
| `fit.delete` | `{"modes"}` | `{"fit"}` |
| `fit.edit` | `{"mode","damping"?,"description"?}` | `{"fit"}` |
| `fit.residual` | `{"show"}` | `{"fit"}` |
| `fit.publish` | `{"name"?}` | `{"name","project","fit"}` |
| `fit.end` | `{}` | `{}` |

`fit.start` fits the FRF `frf`, on the `records` given (indices from 0) or all of them, or, with `shapes`, edits that shape set's fit (Edit Fit): its
modes are adopted as confirmed, on the records whose DOFs the set holds. Records that leave a
response without a record against some reference are refused, since a missing pair is not a
measured zero; so is a complex set, or one with no value at any record's DOFs. A set's DOFs the fit
does not cover are kept when it publishes, and a new mode cannot be confirmed while there are any.
A suggestion is made at once, so the pending mode already has a
frequency and damping. `fit.find` moves the pending mode to the most prominent peak left in the
residual CMIF between `low` and `high` in Hz, the frequencies on screen (the whole range when left
out): the zoom is the search band. `fit.move` puts the cursor at `frequency`, within the measured
range, and searches the damping there; `fit.damping` holds the damping at a fraction of critical
(0.0001 to 0.5, as `fit.edit` takes it) until the next suggestion.
`fit.fit` fits the pending mode without adding it (the preview), so its synthesis can be judged,
and `fit.confirm` adds it. `fit.refine` fits every confirmed mode's residues again, together.
`fit.delete` takes back the confirmed modes at `modes` (indices into `modes`, from 0) and drops
the preview. `fit.edit` types a confirmed mode's `damping` or `description` into its row; a damping changes the synthesis and the residual. `fit.residual` draws the residual's own CMIF in the view, or stops. With
`coherence`, the name of a coherence or multiple coherence in the project, `fit.start` weights each
response at each line by γ²/(1-γ²) in the frequency and damping search, the residues and `fit.refine`,
and `fit.find` reads the residual CMIF with each response at its median weight (`ModalFitSession`).
`fit.publish` adds the confirmed modes to the project as a shape set named `name` (default
"<frf> Modes"), or replaces the set the fit was started from; later publishes replace the set
published. The journal holds the `Project.fit_modes(at=...)` line that replays it. A step the core refuses is refused in its words; a fit whose FRF has left the project
ends.

`fit` is `{"frf","shapes","pending":{"frequency","damping","held"},"preview","modes":[...],"view"}`.
`shapes` is the set the fit edits or last published, or null. `preview` is null or the previewed
mode's `{"frequency","damping"}`; each of `modes` is `{"frequency","damping","description"}` in
frequency order, damping a fraction of critical. `view` is a `spectrum` view, frames carrying its
arrays as `op.run` does: x `Frequency` Hz, y `CMIF` log, `link` `frequency`, the measured CMIF's
first four singular values (`CMIF 1`...), the synthesis's (`CMIF 1, synthesized`...) each
`follows` its measured line, and the pending mode's curve (`Pending mode`, role `reference`).
`mac` is a `matrix` view of the confirmed modes' auto-MAC with the preview's row and column last
(`New: <f> Hz`), how alike the shapes are; null for fewer than two. `coherence` names the
coherence the fit is weighted by, or null; `residual` says whether the view draws the residual's
CMIF (`CMIF 1, residual`..., role `drive`). `half_power` is the half-power reading of the residual
CMIF peak under the cursor: `{"frequency","low","high","q","damping","notes"}`, each of `low`,
`high`, `q` and `damping` null where it could not be read, and `notes` saying why.

**Run log.** The log keeper's run log while a test
runs. A connection holds at most one log open; its lock is released when the log is closed or the
connection ends, and a log another writer holds opens read-only, saying why.

| Method | Params | Result |
|---|---|---|
| `runlog.new` | `{"path","title"?,"mirror"?}` | `{"log"}` |
| `runlog.open` | `{"path","mirror"?}` | `{"log"}` |
| `runlog.close` | `{}` | `{"log":null}` |
| `runlog.reload` | `{}` | `{"log"}` |
| `runlog.state` | `{}` | `{"log"}` (null with none open) |
| `runlog.form` | `{"form","axis"?,"target"?}` | `{form...,"taken","time","preview"}` |
| `runlog.preview` | `{"form","values","axis"?,"target"?}` | `{"text"}` |
| `runlog.write` | `{"form","values","text"?,"time"?,"taken","author","axis"?,"target"?}` | `{"log","said"}` |
| `runlog.export` | `{"path","view","test"?}` | `{"path"}` |

`log` is the window's view: its `path`, `title`, `writable` and `read_only` (why it cannot be
written), the `status` the strip reads (the current test, its axis, run, level and outcome, when
the run started and stopped), the day's `rows` (each entry's `id`, `kind`, `test`, `date`, `line`,
the `corrections` that changed it, and when it was entered if `late`), the `tests` with their
states, the `holds` and `owed` lists, the current test's `header`, the `run` a level row goes into,
the `level` it offers, and the `readings` forms the test's runs take.

A `form` is one of: `level row`, `abort`, `note`, `clock check`, `person in`, `person out`, `run
start`, `run stop`, `cause found`, `correct` (with the entry's id as `target`), `open test`,
`roll`, `close test`, `release`, `header`, `sweep`, `shot`, `result`, `notch`, `deviation`,
`anomaly`, `disposition`, `setting change`, `opinion` and `new log`. `runlog.form` describes it:
its `title`, its `fields` (each `field`, `label`, `box` kind, `choices` for a choice and the
`value` it opens with; a `heading` box titles a group), the `required` fields, the label of its
line of free `text` ('' for none), a `note`, whether it takes an event time (`with_time`) and
whether it computes something as it is typed (`live`). It is opened at `taken`, which `time` shows
as HH:MM:SS. Each box's text goes back as typed in `values`, by field, and the service reads it by
its box kind; `preview` is what a `live` form computes from it (the projection to full level, a
sweep's current against its rating, a shot's checks and pulses, a notch's place in the plan, the
hold a deviation answers), with the predictions drawn for the connection's project beside what is read: the notch Notch Prediction cut beside a notch, and the drives Drive Prediction asked for beside the level row. `runlog.write` writes it as its entries, the event time read from
`time` against `taken`, every entry checked before any is written; a refusal says why. `said` is
what the window says after (an abort's owed first minute, an unplanned notch's hold), or ''.

`runlog.export` writes one view of the open log to `path`: `view` `text` (the log as it reads, every entry in the order written, corrections struck through and restated), `csv` (every entry as stored, corrections included, one row each) or `machine` (the trips and machine events for the lab's own machine log); `test` is a test number for that test alone, or `all` (the default) for the whole log. The view's extension is added when the path has none. The answer is the `path` written.

**Reports.** The report on the connection's project, as the core writes it.

| Method | Params | Result |
|---|---|---|
| `report.generate` | `{"template"?,"name"?}` | `{"name","project"}` |
| `report.render` | `{"name","theme"?}` | `{"name","html"}` |
| `report.views` | `{"name"}` | `{"name","title","marking","css","blocks"}` |
| `report.export` | `{"name","path","anyway"?}` | `{"path","owed","index","note"?}` |
| `report.editor` | `{"name","selected"?,"theme"?,"page"?}` | the editor (below) |
| `report.operate` | `{"name","op",...,"selected"?,"theme"?}` | the editor, and `"project"` |
| `report.print` | `{"name","anyway"?}` | `{"html","owed","index"}` |
| `report.issue` | `{"name","step","path"?,"anyway"?}` | `{"html","status"}`, `{"index"}`, `{"entry"}` or `{}`, and `"project"` |

`report.generate` builds a report from a starter template bound by type to the project's objects
(`Project.generate_report`): `template` is `modal` (the default), `random`, `shock`,
`transient`, `sine`, `sysid`, `mimo`, `model`, `specify`, `signals` or `empty`, and `name`
defaults to "Report". The Report module starts from the template of the module the person came
from: Model's is `model`, Specify's `specify`, Signals' `signals`, and each test module's its own. `report.render`
gives the report `name` as one HTML document, with its stylesheet, data and viewers inline and
nothing loaded from a network, in the `light` or `dark` theme (the reader's own when left out); the
shell shows it in a sandboxed frame. `report.views` gives the same report for the shell's own
page to draw with its viz kit, with `css`, the stylesheet the report adds (the printed page's
margins when the report identity is on; empty otherwise): each drawn block is `{"kind":"text","html"}`,
`{"kind":"figure","views"}` (the protocol's views, arrays as binary frames) or
`{"kind":"unbound","was"}`, with `block` (the index of the report block it came from, shared by a
block's several figures), `label` (its figure or table number), `caption` and an optional `note`.
`report.export` writes the page that is printed and issued,
light, to `path` (`.html` added when it has another suffix), beside the target first and renamed
over it. First it makes the checks asked before a report leaves the project: with skeleton sections
on, the sections (a flash report's parts) the report still owes; with the data index on, every
indexed file hashed again and those that do not match the checksum indexed. A report that owes
or whose files have `CHANGED` or are `NOT FOUND` is not written unless `anyway` is true; the
answer then carries `owed` (the sections' lines) and `index` (`{name, status}` a file) with
`path` null, and the shell asks "Export anyway?". Written, the answer is the `path` with both
lists empty and a `note` saying how many unbound blocks the page left out and what the index check
found ('' when neither applies). An issued report that has changed since is refused: a change after issue is an
amendment. What is compared leaves out the page's line on the last check of the files and its line
on the program and the project file, so checking again or saving the project is not a change.

`report.editor` gives the report editor: `html`, the core's edit page (every block
tagged with its index, the block at `selected` framed, and unbound blocks shown as cards to bind),
and the pane for the selection. A click on a block posts `{"hforgeReport":{"op":"select","at":n}}`
(`at` null outside every block) to the page's parent, which the shell answers with
`report.editor` at that block and `"page":false`: the page has framed the block itself, so the
answer leaves `html` and `references` out and nothing is rendered again. `title`, `marking` and `marking_color` (`ink` or `red`) are the
report's; `blocks` how many it has; `insert` the Insert menu as `[kind, label]` pairs (Markdown,
then `core.report.insert_options`); `references` the `[label, caption]` of each numbered figure
and table, for a text block's `{{figure:<caption>}}` token. `block` is null with nothing selected,
or the selected block's `index`, `kind` and `mode`, and either its `text` (a text block; `special`
names a skeleton section, check or requirement block, which has a pane of its own) or its
`fields` (`{field,label,value,options:[[value,label]...]}`, the objects it may name, symbolic
bindings such as `@basis:ShapeSet` first) and `caption`.

`report.print` gives the reader's page as it is printed: light, its figures drawn at print
resolution (`#print`), and asking for the print dialog once they are drawn, where the reader saves a
PDF. Its print stylesheet carries the margins the identity rides in. It makes `report.export`'s
checks first and answers the same way: `html` null with `owed` and `index` until `anyway` is true.
An issued report that has changed since is refused. `report.issue` is the Issue in three steps
around that print. `begin` checks the report can be issued (Report Identity on, every required
field entered, not already issued), makes the data index check `report.export` makes (answering
`{"index":[...]}` alone when files are astray and `anyway` is not true), and gives the page to
print, which says issued (or the amendment), and the `status`. The report itself stays a draft, so
a save in between writes a draft. `record` reads the PDF saved from the page at `path`, marks the
report and records the issue with the SHA-256 of the PDF and of the page; its `entry` is the
record's new line. `cancel` forgets the page. A file that holds a status of issued or amended with
no record behind it opens as a draft. The summary's `settings` for a report carry its `identity` fields
as text beside its `features`.

`report.operate` applies one of the editor operations and answers with the editor at the block it
leaves selected: `insert` (`kind`, one of `insert`'s, at `at` or the end, bound to the first
object the project has for it), `remove` (`at`), `move` (`from`, to before `to`), `text` (`at`,
`value`), `field` (`at`, `field` `caption` or one of the block's `fields`, `value`), `title`,
`marking` and `marking_color` (`value`).

**Control service**

The hardware and Random methods are in "Hardware and Random Control" below. The `sim.*` methods
came first and are superseded for running tests: `random.*` on the `sim` station runs the same
closed loop with the full configuration, controller ownership and the safe stop. They stay,
unchanged, for the Simulate workspace's demonstrations (they alone run faster than real time).

| Method | Params | Result |
|---|---|---|
| `sim.profiles` | `{}` | `{"profiles":[{"id","title","summary"}]}` |
| `sim.start` | `{"profile","level_db","speed"}` | `{"run_id"}` (`speed` 1 = real time, 0 = as fast as possible) |
| `sim.stop` | `{}` | `{"state"}` |
| `sim.state` | `{}` | `{"state":"idle"|"running"|"stopped","run_id"|null}` |

`speed` is a multiple of real time from 0 to 1000; 0 means as fast as the engine runs.

The service runs one test at a time. `sim.start` is refused with reason `run_active` while a
`random.*` run is live (armed, running, holding for review or stopping), and `random.arm` and
`random.start` are refused the same way while a `sim.start` run is running. `sim.stop` and `sim.state` see only runs that
`sim.start` started.

Event topic `random.display`, about 4 to 10 per second while running:
`{"run_id","source":"SIM","state","elapsed_s","level_db","views":[spectrum, reading],
"phase","outcome","reason"}`. `phase` is the controller's own phase name. Once `state` is
`stopped`, `outcome` is one of `completed`, `operator_stop`, `aborted`, `held`, `fault`,
`law_fault`, and `reason` says why in words; the shell shows any outcome other than `completed` or
`operator_stop` as an abort with that reason.
`source` is always present on control data; the shell shows it as a badge. A simulated run is
never presented as a hardware result.

## Hardware and Random Control

The control service runs one test at a time on one station. The methods below select the
station (the simulator or NI-DAQmx hardware), edit the channel table, configure a closed-loop
Random test, run it, and show it live. The control loop runs on the service's own thread beside
the hardware. The socket carries the operator's intent down and display data back, and never a
sample of the loop.

A stop or an abort sent over the socket is a request. The controller's own protection (the
envelope, kinematic, drive and divergence aborts, and the station's watchdog) and the
amplifier's emergency stop are the abort authority, and the shell labels its Stop and Abort
accordingly.

Every value from a run carries `source`: `SIM` or `HARDWARE`. Only physical NI devices give
`HARDWARE`. The simulator and NI's driver-simulated devices both give `SIM`, and `class` says
which. The shell shows `HARDWARE` as its live badge and `SIM` as the simulated badge, and names
driver-simulated devices as such. Nothing simulated is ever presented as a measurement.

### Refusals

Every error from a method in this section carries `reason`, a stable name a client branches on.
`message` is the words a person can act on and is shown verbatim; a client never parses it. It is
plain words: it never names a protocol method.
`detail`, when present, carries the facts named below.

```
{"type":"response","id":12,"error":{"code":"refused","reason":"controller_held",
 "message":"Another client controls this service (Shell on bench-pc). Close that client, or wait for it to release control.",
 "detail":{"holder":"Shell on bench-pc"}}}
```

| `reason` | `code` | Cause | `detail` |
|---|---|---|---|
| `bad_params` | `bad_request` | A parameter is missing, of the wrong type, or not one of its stated values | `param` |
| `not_controller` | `refused` | A method that changes the station, the table or the run (other than `random.stop` and `random.abort`), from a connection that does not hold control | `holder` (null when nobody does) |
| `controller_held` | `refused` | `session.claim` while another connected client holds control | `holder` |
| `run_active` | `refused` | The call would change what a live run (armed, running, holding for review or stopping) depends on, or start a second run | `run_id` |
| `not_configured` | `refused` | `random.arm` or `random.start` with no configuration | |
| `run_finished` | `refused` | `random.arm` or `random.start` on a finished run; reset or configure first | `outcome` |
| `wrong_state` | `refused` | The verb does not apply in the run's present state | `state`, `phase` |
| `manual_mode_required` | `refused` | `random.level`, `sine.level` or `mixed.level` while the ladder is automatic | `mode` |
| `not_a_rung` | `bad_request` | `random.level`, `sine.level` or `mixed.level` `to_db` is not a level of the schedule, or `twr.level` names a rung the table does not have or steps past its ends | `levels_db` |
| `feasibility_exceeded` | `refused` | The profile exceeds the shaker's ratings, or a TWR section its feasibility ceilings; the run is blocked until armed with an acknowledgement | `feasibility` |
| `override_not_needed` | `refused` | `override_ack` sent while the run is not blocked | |
| `arm_refused` | `refused` | The station refused to arm: the rate it programmed, the analysis grid at that rate, its sample clock, or the driver. The message is the controller's own | |
| `ni_driver_unavailable` | `refused` | The NI-DAQmx runtime is not installed on this computer | |
| `no_ni_devices` | `refused` | The scan found no NI devices | |
| `scan_failed` | `refused` | The driver refused the scan | |
| `devices_absent` | `refused` | Selected devices are not in the latest scan | `missing`, `found` |
| `mixed_simulated_and_real` | `refused` | The selection mixes driver-simulated and physical devices | `simulated`, `real` |
| `mixed_timing_families` | `refused` | The selection spans devices that cannot share one sample clock | `families` |
| `not_enough_channels` | `refused` | The selected devices have fewer channels than the table uses | `want_inputs`, `want_outputs`, `have_inputs`, `have_outputs` |
| `hardware_busy` | `refused` | Another program holds the NI hardware | `holder` |
| `record_unwritable` | `refused` | `random.arm`, `random.start`, `sine.arm`, `sine.start`, `shock.arm`, `shock.start`, `srs.arm`, `srs.start`, `twr.arm`, `twr.start`, `mimo_random.start`, `explorer.start`, `mixed.arm` or `mixed.start`: the run's record (its folder, `run.json`, `table.json` or `setup.json`) cannot be written in the data folder; nothing is armed | `path`, `reason` |
| `stale_revision` | `refused` | `channels.set` quoted a revision that is no longer current | `current`, `provided` |
| `table_invalid` | `bad_request` | The channel table has errors | `errors`, `warnings` |
| `config_invalid` | `bad_request` | The Random, classical shock, SRS shock, Sine, Mixed Mode, TWR, Explorer or Recorder configuration has errors (for Explorer and the Recorder, also at `explorer.start` or `recorder.start` when the table no longer carries the run) | `errors`, `warnings` |
| `shaker_record_invalid` | `bad_request` | The feasibility shaker record does not parse | |
| `setup_invalid` | `bad_request` | The MIMO setup has errors | the whole validation report |
| `gate_failed` | `refused` | `mimo_random.proceed` after identification failed a gate; `mimo_random.override` with reason `gate_failed` goes on anyway | `gates` |
| `prediction_exceeded` | `refused` | `mimo_random.proceed` while the prediction asks a drive for more than its ceiling; `mimo_random.override` with reason `prediction_exceeded` engages anyway | |
| `identification_mismatch` | `refused` | `mimo_random.identification.load` (or the start that loads it) with a set measured on another setup | `setup_hash`, `identification_hash` |
| `law_import_failed` | `refused` | The file is not a control law this program can run, or cannot be read | `path` |
| `catalog_unavailable` | `refused` | The control-law catalog in the data folder cannot be read | |
| `law_unknown` | `not_found` | `laws.check` names an id the catalog does not hold | `id` |
| `law_params_invalid` | `bad_request` | `laws.check` parameter values the law does not accept | `id` |
| `law_check_unsupported` | `refused` | `laws.check` names a single-output Random law without `mixed`, or a law the check cannot build, or a case of a check cannot be run at all (the message names it) | |
| `not_yet` | `refused` | `shock.fire`, `shock.train`, `shock.burst` or `srs.fire` before the time between pulses has passed, or while a pulse is in flight | `seconds` |
| `wrong_mode` | `refused` | `shock.fire`, `shock.level`, `shock.train`, `shock.burst`, `srs.fire`, `srs.level`, `twr.iterate` or `twr.play` while the ladder is automatic, or `twr.level` then anywhere but `holding`; `recorder.record` outside the `manual` trigger mode, `recorder.arm` and `recorder.disarm` in the `start` mode | `mode` |
| `drive_ceiling` | `refused` | `shock.level` or `srs.level` to a level where the identified drive would pass the drive abort; `twr.level` to a level where the drive in force would pass the drive ceiling | `vpk`, `abort_vpk` (for TWR `ceiling_vpk`, and `vpk` null when the service cannot state it), `level_db` |
| `reference_unreadable` | `refused` | `twr.reference.import`: the file cannot be read, or its content is not a record (the message names the line or field) | `path` (null for pasted text), `line` (null when no line is at fault) |
| `not_converged` | `refused` | `twr.play` before the present level has gated | `level_db`, `gate_pct` |
| `coherence_hold` | `refused` | `twr.proceed`, `twr.iterate` or `twr.play` while the coherence hold holds the run; `twr.reset` alone releases it | `coherence_mean` (null when not measured), `min_mean_coherence` |
| `level_zero` | `refused` | `explorer.steer` with `step_db` while the target level is 0 V: a step in dB has nothing to step from; set a level in volts first | |
| `not_enough_space` | `refused` | `recorder.start` while the data volume's free space is under the run's free-space floor | `free_bytes`, `floor_bytes` |
| `not_a_sweep` | `refused` | `sine.reverse`, `sine.jog` or `sine.goto` when the test is not a sweep | `test` |
| `out_of_band` | `refused` | `sine.goto` to a frequency outside the test's band | `band_hz` |
| `adjust_refused` | `refused` | `sine.adjust` or `mixed.adjust` of a parameter that is not one of the law's live parameters, or out of its range, or that the law refused | `key`, `why` (`unknown_key`, `not_live`, `out_of_range` or `law_refused`) |
| `part_refused` | `refused` | `mixed.part` naming no part of the run, switching off the floor, or switching on a part whose window has closed or whose schedule has ended | `id`, `why` (`unknown_part`, `floor`, `window_closed` or `schedule_ended`) |
| `law_check_running` | `refused` | `laws.check` while another check runs | |
| `law_check_not_recorded` | `refused` | The check ran but its record could not be written | |
| `law_not_checked` | `refused` | `mimo_random.start`, `shock.arm`, `srs.arm`, `sine.arm`, `mixed.arm` or `twr.arm` on a hardware station with a law file that has no passed check for its exact file (the simulator runs any law; built-in laws are cleared), for `mixed.arm` also no passed mixed check in its role, or for `twr.arm` a passed check of an earlier check version; run `laws.check` first | `twr.arm` with an earlier check: `tool_version`, `current`; `mixed.arm`: `role` (`floor` or `tones`) |
| `shutting_down` | `refused` | The control service is shutting down (see "Shutting Down"): `random.arm`, `random.start`, `random.proceed`, `random.level`, `random.mode`, `sim.start`, `shock.arm`, `start`, `proceed`, `fire`, `level`, `train` and `burst`, `srs.arm`, `start`, `proceed`, `fire` and `level`, `sine.arm`, `start`, `proceed`, `level`, `mode`, `resume`, `reverse`, `jog`, `goto` and `adjust`, `mixed.arm`, `start`, `proceed`, `level`, `mode`, `resume`, `part` and `adjust`, `twr.arm`, `start`, `proceed`, `iterate`, `play` and `level`, `explorer.start`, `steer` and `sweep`, and `recorder.start`, `arm` and `record` are refused | |

`errors` and `warnings` are lists of items, `{"reason","message"}` plus whichever of `channel`
(a table row's `index`), `row` (a breakpoint row, from 0) and `field` (a configuration field
path such as `control.damping`) locates the problem. Item reasons:

| Where | Errors | Warnings |
|---|---|---|
| Channel table | `duplicate_index`, `no_channel_enabled`, `zero_sensitivity`, `non_positive_range`, `control_group_gap`, `reference_without_control`, `unsupported_schema`, and on an NI station `channel_unbound`, `physical_channel_unknown`, `physical_channel_wrong_direction`, `physical_channel_shared`, `mixed_timing_families` | `iepe_without_bias`, `bias_on_single_ended`, `reference_count_mismatch`, `drive_channel_present` |
| Profile | `profile_empty`, `first_row_not_anchor`, `row_level_ambiguous`, `frequency_not_positive`, `frequency_not_ascending`, `psd_negative` | `alarm_wider_than_abort` |
| Acquisition | `n_pts_not_power_of_two`, `band_above_grid`, `too_few_lines` | |
| Schedule | `levels_empty`, `levels_not_ascending`, `dwell_missing` | |
| Control and limits | `no_control_channel`, `weight_unknown_channel`, `weights_sum_not_positive`, `limit_unknown_channel`, `limit_rows_not_ascending` | |
| Any number | `out_of_range` (`field` names it, `message` states the range) | |
| MIMO channels and control space | `control_unknown_channel`, `drive_unknown_channel`, `duplicate_channel`, `no_control_channel`, `no_drive_channel`, `transform_shape`, `transform_rank_deficient`, `backend_not_selected` | `transform_ill_conditioned`, `alarm_wider_than_abort` |
| MIMO reference | `reference_shape`, `reference_too_large_inline`, `reference_not_hermitian`, `reference_not_psd`, `too_few_lines`, `frequency_not_ascending` | `reference_repaired`, `reference_rank_above_drives`, `reference_file_unread` |
| MIMO law | `law_unknown`, `law_wrong_world`, `law_shape_not_supported`, `law_param_unknown`, `law_param_invalid` | |
| Classical shock | `out_of_range`, `levels_empty`, `levels_not_ascending`, `pulses_length_mismatch`, `band_above_grid`, `record_too_long`, `spacing_below_floor`, `limit_unknown_channel`, `no_control_channel`, `no_drive_channel`, `extra_drive_channels`, `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid` | `quiet_window_exceeded` |
| SRS shock | `out_of_range`, `levels_empty`, `levels_not_ascending`, `pulses_length_mismatch`, `band_above_grid`, `bands_empty`, `target_not_covering`, `target_not_ascending`, `alarm_wider_than_abort`, `per_band_length_mismatch`, `components_engine_mismatch`, `component_invalid`, `record_too_long`, `limit_unknown_channel`, `limit_no_frf`, `no_control_channel`, `no_drive_channel`, `extra_drive_channels`, `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid`; `target_file` and `table_file` from the imports | `synthesis_not_converged`; `target_file` (an ignored column) from the target import |
| Sine | `out_of_range`, `profile_too_short`, `frequency_not_positive`, `frequency_not_ascending`, `value_not_positive`, `quantity_ambiguous`, `profile_not_covering`, `band_above_grid`, `levels_empty`, `levels_not_ascending`, `start_not_a_level`, `end_before_full_level`, `tick_out_of_range`, `abort_below_ceiling`, `no_control_channel`, `weight_unknown_channel`, `no_drive_channel`, `limit_unknown_channel`, `limit_on_control_channel`, `cola_needs_two_outputs`, `safe_stop_budget`, `tracking_lag`, `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid`; `profile_file` from the import | `join_discontinuity`, `alarm_wider_than_abort`, `tracking_lag`; `units_column_ignored` from the import |
| Mixed Mode | Random's profile, acquisition, schedule and control reasons for the floor and the ladder, `out_of_range`, `no_floor`, `too_many_floors`, `unknown_part_kind`, `duplicate_part_id`, `part_out_of_band`, `parts_too_close`, `too_many_tones`, `too_many_bands`, `follow_unknown_part`, `follow_self`, `follow_loop`, `schedule_unsupported`, `profile_too_short`, `value_not_positive`, `profile_not_covering`, `tick_out_of_range`, `abort_below_ceiling`, `no_control_channel`, `weight_unknown_channel`, `no_drive_channel`, `limit_unknown_channel`, `tracking_lag`, `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid` | `alarm_wider_than_abort`, `join_discontinuity`, `tracking_lag` |
| TWR | `out_of_range`, `reference_unknown`, `reference_changed`, `units_mismatch`, `rate_mismatch`, `section_outside_reference`, `record_too_long`, `band_above_grid`, `band_outside_identification`, `highpass_above_band`, `identification_too_short`, `levels_empty`, `levels_not_ascending`, `alarm_wider_than_abort`, `no_control_channel`, `no_drive_channel`, `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid` | `reference_resampled` |
| Explorer | `out_of_range`, `band_above_grid`, `ceiling_over_range`, `safe_stop_budget`, `no_drive_channel`, `extra_drive_channels`, `no_input_channel` | |
| Recorder | `out_of_range`, `not_power_of_two`, `row_not_recorded`, `frf_same_row`, `no_input_channel` | `drive_channel_present` |

A refusal not listed here is a defect in the service.

### Controller and Observers

One connection at a time holds control; any number observe. Every connection starts as an
observer. Observers receive every event and may call every method that only reads
(`session.state`, `station.list`, `channels.get`, `random.state`, `random.config`, `random.log`,
`random.subscribe`, `mimo_random.status`, `mimo_random.validate`, `mimo_random.report`,
`mimo_random.subscribe`, `laws.list`, and the `dry_run` checks).

**Stopping is open to everyone.** `random.stop` and `random.abort` (and MIMO Random's
`mimo_random.stop` and `mimo_random.abort`) are accepted from any connection, controller or
observer. They only ever end a run, so a second client using them
cannot leave the run in a state nobody asked for, and whoever is watching a test that goes wrong
can end it from the screen in front of them. `random.stop` is the gentler of the two, so whoever
may abort may also stop. The control service listens on loopback only, so every client is on
the computer that runs the test. Every other method in this section changes what runs or how,
and is refused `not_controller` from an observer: one client at a time decides what the test
does. The run log and `end.message` name the client that stopped or aborted the run.

| Method | Params | Result |
|---|---|---|
| `session.state` | `{}` | `{"role","controller","orphan_s","heartbeat_s","timeout_s"}` |
| `session.claim` | `{"name","lease"}` (both optional) | `{"role":"controller","lease","resumed","heartbeat_s","timeout_s"}` |
| `session.release` | `{}` | `{"role":"observer","orphan_s"}` |

`role` is `controller` or `observer`. `controller` is `{"name","present"}`, or null when nobody
holds control. `name` is how other clients see the holder; it defaults to the peer address.
`orphan_s` is null, or the seconds left before an unattended run is stopped (below). A claim
while another connected client holds control is refused `controller_held`; there is no forced
takeover.

**Heartbeat.** The service sends a WebSocket ping every `heartbeat_s` (5 s). Any inbound frame
shows a connection is alive, and browsers answer pings on their own, so a client sends nothing
for it. After three heartbeats with nothing inbound, the connection is treated as gone.

**When the controlling client goes.**

- **Without a goodbye** (the network drops, the computer sleeps): the run continues, and the
  seat is kept for `timeout_s` (30 s) with `controller.present` false and `orphan_s` counting
  down. A `session.claim` that presents the same `lease` in that time resumes control
  (`resumed: true`); a claim without it takes control from the absent holder.
- **With a goodbye** (a clean close, or `session.release`): control is free at once, and a live
  run is kept for `timeout_s` for a client to claim.
- **Nobody claims in time:** if a run is live when `orphan_s` reaches 0, the service ends it
  with the controlled stop, the same path as `random.stop` (while driving, the level ramps down
  over the configured `shutdown_ramp_s`), never with an abort. The run's outcome is
  `safe_stop`.

`session.changed` (text event, to every connection on every change):
`{"role","controller","orphan_s"}`. `role` is the receiving connection's own (`controller` or
`observer`), so each connection learns from the event whether it holds control; every connection
receives its own copy.

### Stations

| Method | Params | Result |
|---|---|---|
| `station.list` | `{"rescan"}` (optional, default false) | `{"stations":[sim, ni],"selected"}` |
| `station.select` | `{"kind":"sim"}` or `{"kind":"ni","devices":[id...]}` | `{"selected"}` |

The service starts with the simulator selected. It never selects hardware on its own, and never
falls back to the simulator when hardware is missing, busy or refused: the selection is refused
by name instead.

```
{"kind":"sim","title":"Simulator","available":true,"class":"sim","inputs":8,"outputs":1,"clock":"shared"}
{"kind":"ni","title":"NI-DAQmx","available":true,"unavailable":null,"scanned_unix":1793577600,
 "devices":[{"id":"cDAQ1Mod1","product":"NI 9234","serial":"01A2B3C4","simulated":false,
             "parent":"cDAQ1","slot":1,"timing_family":"cDAQ1",
             "inputs":[{"id":"ai0","label":"ai0"}],"outputs":[]}]}
```

- The simulator is a fixed plant with two resonances (near 200 and 550 Hz) on one shared sample
  clock, paced in real time at the configured rate.
- `available` false on `ni` comes with `unavailable: {"reason","message"}`, using the station
  reasons above (`ni_driver_unavailable`, `no_ni_devices`, `scan_failed`).
- `simulated` is the driver's own statement that no physical device is behind the id. It is
  never inferred from a name.
- A chassis is listed as a device without channels; its modules name it as `parent` with their
  `slot`.
- `timing_family` groups the devices that can share one sample clock: every module in one
  chassis is one family (named by the chassis), and a standalone device is its own family. All
  of a run's channels must be in one family.
- The scan does not read a module's sample-rate ladder, IEPE capability or output range, so none
  is reported. The rate the hardware actually runs at is reported when the run arms. A known
  limit: a client cannot tell from the scan whether a module supports IEPE; a row set to `iepe`
  on a module without it is refused by the driver when the run arms (`arm_refused`).

`selected`:

```
{"kind":"ni","devices":["cDAQ1Mod1","cDAQ1Mod2"],"class":"ni_real","source":"HARDWARE",
 "inputs":4,"outputs":4,"timing_family":"cDAQ1"}
```

`class` is `sim` (the simulator's plant), `ni_simulated` (driver-simulated NI devices: the real
driver and tasks, no physical signal) or `ni_real` (physical NI devices). `source` follows from
it. `devices` is empty and `timing_family` null for the simulator.

`station.select` checks the selection against a fresh scan. Refusals: `bad_params` (an unknown
kind, or `ni` with no devices), `not_controller`, `run_active`, `ni_driver_unavailable`,
`no_ni_devices`, `scan_failed`, `devices_absent`, `mixed_simulated_and_real`,
`mixed_timing_families`, `hardware_busy`. Selecting opens nothing: devices are opened when a run
arms. `station.changed` (text event, to every connection when the selection or the scan
changes) carries the `station.list` result.

### Channel Table

One table, held by the control service, used by every run.

| Method | Params | Result |
|---|---|---|
| `channels.get` | `{}` | `{"revision","channels":[row...],"report"}` |
| `channels.set` | `{"revision","channels":[row...],"dry_run"}` (`dry_run` optional) | `{"revision","report"}` |

```
{"index":1,"enabled":true,"description":"Control","role":"control",
 "physical":{"device":"cDAQ1Mod1","channel":"ai0"},"interface":"single_ended","iepe_bias_v":null,
 "units":"g","sensitivity_mv_per_eu":100,"range_max":50,"polarity":"positive",
 "control_group":null,"abort":{"accel_g":null,"velocity_ips":null,"displacement_mil":null}}
```

| Field | Values | New row |
|---|---|---|
| `index` | integer, unique in the table; the channel's number everywhere else | |
| `enabled` | boolean | true |
| `description` | text | empty |
| `role` | `control` (closes the loop), `monitor`, `drive` (an output), `reference` | `monitor` |
| `physical` | `{"device","channel"}` from the scan, or null (unbound) | null |
| `interface` | `single_ended` or `iepe` | `single_ended` |
| `iepe_bias_v` | number, or null | null |
| `units` | `g`, `in/s`, `mil`, `lbf`, `V`, or another unit's name | `g` |
| `sensitivity_mv_per_eu` | number, mV per engineering unit; not zero on an enabled input | 100 |
| `range_max` | number above 0, full scale in the row's unit | 100 |
| `polarity` | `positive` or `negative` | `positive` |
| `control_group` | integer from 0 with no gaps, or null | null |
| `abort` | `{"accel_g","velocity_ips","displacement_mil"}`, each a limit or null | all null |

Every row carries every field. A row's `abort` limits are absolute: a run aborts when that
channel passes one, at any level.

**IEPE is off unless a client turns it on for a row.** The service's starting table (one
control input, one drive output, both unbound) and every row it creates are `single_ended`, and
the service never turns IEPE on by itself. IEPE drives a constant current into the input: an
input wired to a voltage source, such as a loopback from an output or a signal generator, stays
`single_ended`.

**Validation.** `report` is `{"errors":[item...],"warnings":[item...]}` with the item reasons
listed under Refusals. On the simulator, `physical` is kept but not used or checked. On an NI
station, every enabled row must be bound to a channel of the selected devices in the right
direction (an input row to an input, a drive row to an output), no two rows to one channel, and
all in one timing family. Selecting another station checks the table again: `channels.get`
reports the errors until the table is fixed, and `random.configure` is refused `table_invalid`.

**Revisions.** Every write increments `revision`. A `channels.set` that quotes an older revision
is refused `stale_revision`; the client reads the table again and shows the operator what
changed before sending, and never retries on its own, because the table carries the abort
limits. A table with errors is refused `table_invalid` and not written; warnings never block.
`dry_run: true` checks the table and returns the report without writing; it needs no control and
is never refused for the table's content. `channels.changed` (text event, to every connection
after a write or a station change) carries the `channels.get` result.

### Random Test Configuration

| Method | Params | Result |
|---|---|---|
| `random.configure` | `{"config","dry_run"}` (`dry_run` optional) | `{"state","derived","warnings"}` |
| `random.config` | `{}` | `{"run_id","config","derived","views"}` |

```
{"title":"Qualification Random",
 "profile":{"breakpoints":[{"freq_hz":20,"psd_g2_hz":0.01},{"freq_hz":80,"slope_db_oct":3},
                           {"freq_hz":350,"psd_g2_hz":0.04,"alarm_db":[2,2]},
                           {"freq_hz":2000,"slope_db_oct":-3}],
            "alarm_db":[3,3],"abort_db":[6,6]},
 "acquisition":{"fs_hz":8192,"n_pts":2048,"n_avg":4},
 "schedule":{"mode":"auto","levels":[{"db":-12,"dwell_s":30},{"db":-6,"dwell_s":30},{"db":0}],
             "full_level_s":300},
 "control":{"review_after_sysid":true,"shutdown_ramp_s":3},
 "limits":[],"feasibility":null}
```

**Profile.** The reference, in g²/Hz. Each breakpoint row is either a PSD anchor
(`psd_g2_hz`) or a slope from the row before (`slope_db_oct`), never both; the first row is an
anchor and frequencies rise strictly. The test band runs from the first row to the last.
`alarm_db` and `abort_db` are `[below, above]` in positive dB: the band runs from the reference
times 10^(-below/10) to the reference times 10^(above/10). A row's own bands hold from that row
to the next; elsewhere the profile's bands hold (defaults `[3,3]` and `[6,6]`). An alarm band
wider than its abort band is a warning.

**Acquisition.** `fs_hz` is the rate requested of the station; hardware may run at a nearby
rate, and the run reports the rate it actually ran at. `n_pts` is the FFT length (a power of
two) and `n_avg` the averages per control loop. The lines are `fs_hz / n_pts` apart. A profile
whose top breakpoint the grid cannot reach is refused `band_above_grid`, and fewer than 8 lines
in the band is refused `too_few_lines`; both are checked again at arm against the rate the
hardware programmed.

**Schedule.** `levels` are dB relative to the reference, in non-decreasing order; the last is
full level. Every level but the last has a `dwell_s`. In `auto`, the run starts at the first
level, holds each for its dwell, holds full level for `full_level_s`, and ends `completed`. In
`manual`, the run starts at the first level and moves only on `random.level`, between the first
and last levels, until stopped; `full_level_s` applies if the ladder is handed back to `auto`.

**Control.** Every field is optional:

| Field | Default | Meaning |
|---|---|---|
| `composite` | `weighted_avg` | How the control channels (the table's enabled `control` rows) combine: `weighted_avg`, `min` or `max` |
| `weights` | 1 each | `[{"channel","weight"}]` for control rows |
| `control_dof` | 120 | Statistical degrees of freedom of the control estimate; higher is steadier and slower to adapt |
| `damping` | 4 | Correction damping, 0 (correct fully every loop) to 10 |
| `max_lines_out` | 10 | Lines outside the abort band in one loop that trip the envelope abort |
| `max_loops_out` | 5 | Consecutive diverging loops that trip the divergence abort |
| `max_sigma` | 5 | Drive clip, in standard deviations; null for no clip |
| `drive_ceiling_vrms` | 10 | The drive is held at or below this rms |
| `drive_abort_vrms` | null | A drive rms at or above this aborts; null for none |
| `coherence_floor` | 0.5 | Band coherence the system identification must reach |
| `review_after_sysid` | true | Hold after system identification for the operator to proceed |
| `pretest` | `{"averages":4,"level_db":-20,"max_vrms":1,"start_vrms":0.01,"shaped":false}` | System identification (below); `shaped` follows the reference's shape instead of a flat band |
| `soft_start_db` | 9 | The run engages this far below the first level and ramps up to it |
| `soft_start_relax_db` | 6 | The bands widen by this much until the first level is reached |
| `level_ramp_s` | 3 | Ramp time of every level change: the drive moves smoothly, output block by output block, while the control loop keeps its own cadence |
| `shutdown_ramp_s` | 3 | Ramp time of a controlled stop: 40 dB down from the level the stop began at, smoothly, block by block |

**The control estimate.** The loop controls to, and the envelope judges, an estimate of the
control spectrum accumulated over loops to `control_dof` degrees of freedom (each loop adds
`2 × n_avg`). A level change keeps it, scaled to the new level; a loop measured while the level
moves is folded in at the level it reached, and is not judged. The new level is also judged for
the envelope abort on its own estimate, from its first loop at rest, so an excursion at a new
level aborts as soon as that estimate holds 16 degrees of freedom (the first loop at `n_avg` 8
or more). No envelope alarm or abort is judged on fewer than 16 degrees of freedom.

**System identification.** It excites relative to the reference. It starts at `start_vrms` and
steps its drive until the control response's rms over the band is `level_db` (-60 to 0) relative
to the reference's rms over the band, within 1 dB, then measures the system over `averages`
control loops at that drive. The drive never exceeds `max_vrms` (null: no ceiling of its own)
or `drive_ceiling_vrms`. When the response cannot reach its level within that, or does not settle
on it, the run ends `sysid_level_unreachable` without driving further. `start_vrms` is greater
than 0 and at most `max_vrms`. The run's aborts (each channel's `abort` limits, and
`drive_abort_vrms`) are armed from the first identification drive, exactly as in the control
loop.

**Limits.** `[{"channel","kind","rows":[{"freq_hz","ceiling"}...]}]`, `kind` `response` or
`force`, `ceiling` a PSD in the channel's unit squared per Hz. The controller lowers the drive
where the channel would pass its ceiling. A limit never aborts.

**Feasibility.** Optional `{"shaker","moving_mass_lb","crest_factor"}`: `shaker` is a shaker
record in the controller's shaker-database form, `moving_mass_lb` is `{"armature","slip_table",
"drive_adapter","fixturing","test_article"}`, `crest_factor` defaults to 3. Checked at arm.
Without it, feasibility is not evaluated and the run says so.

**The run's record.** Optional, for the run folder (see Run Folders): `operator`, `article` and
`notes` are the operator's words (strings, empty when not given); `specification`
`{"document","clause"}` names what the test is run to; `record` `{"streams"}` (default true)
says whether the record holds the inputs and the drive as streams. None of them changes how the
run drives.

**Result.** `state` is the `random.state` object (`configured`). `derived`:

```
{"grms_g":6.06,"velocity_rms_ips":2.31,"displacement_rms_mil":21.4,"band_hz":[20,2000],
 "grid":{"fs_hz":8192,"n_pts":2048,"df_hz":4,"lines":496,"loop_s":1,"actual":false},
 "control_channels":[1]}
```

`grid` is the requested grid until the run arms (`actual` false); `lines` counts lines in the
band; `loop_s` is the time one control loop takes. A configuration replaces the one before;
it is refused `run_active` while a run is live, and
`config_invalid` or `table_invalid` with the items when it has errors. Configuring checks the
table against the selected station and opens nothing. `dry_run: true` checks without
replacing anything and needs no control; its result is `{"errors","warnings","derived","views"}`
(`derived` null and `views` empty when there are errors).

`views` is `[reference]`, the profile on the grid, so a screen draws it without computing it:
**Reference** (`spectrum`, x `Frequency` Hz log, y `PSD` g²/Hz log, `link` `frequency`), series
in this order: `Reference` (`reference`), `Alarm high` and `Alarm low` (`tolerance`), `Abort high`
and `Abort low` (`abort`), at 0 dB with each row's own bands applied, on the band's lines of the
requested grid, each `shape: "steps"` with its `edges`. A result carrying it is a binary frame.

`random.config` returns what the present run was configured with: its `run_id`, the `config` as
accepted, its `derived` values and the same `views`, so a screen that reloads, or a second
screen, shows the profile and the numbers the run uses. All four are null (`views` empty) when
nothing is configured. Like `random.state` it only reads. `derived.grid` stays the requested
grid; the run state's `grid` is the one the run uses once it arms.

### Running a Random Test

| Method | Params | From | Result |
|---|---|---|---|
| `random.arm` | `{"override_ack"}` (optional) | `configured`, `blocked` | state |
| `random.start` | `{}` | `configured`, `armed` | state |
| `random.proceed` | `{}` | `review` | state |
| `random.level` | `{"delta_db"}` or `{"to_db"}` | `running`, in `manual` | `{"level_db"}` |
| `random.mode` | `{"mode":"auto"|"manual"}` | any configured state | `{"mode"}` |
| `random.stop` | `{}` | any | state |
| `random.abort` | `{}` | any | state |
| `random.reset` | `{}` | any but `running`, `review`, `stopping` | state |
| `random.clear` | `{}` | any but `running`, `review`, `stopping` | state |
| `random.state` | `{}` | any | state |
| `random.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |

A run is live while it is armed, running, holding for review or stopping. "state" in the
Result column is the `random.state` object below.

- **`random.arm`** checks feasibility (when given), opens the station, programs the rate, builds
  the analysis grid at the rate the hardware actually runs at, and passes the controller's arm
  gate (shared sample clock, rate, grid). Nothing is driven. The state becomes `armed`, with
  `grid.actual` true and `clock`. When the profile exceeds the shaker's ratings it is refused
  `feasibility_exceeded` and the state becomes `blocked`; `random.arm` with a non-empty
  `override_ack` then arms past it, and the text is logged verbatim and kept with the run.
  Refusals: `not_controller`, `not_configured`, `run_active`, `run_finished`,
  `feasibility_exceeded`, `override_not_needed`, `bad_params` (an empty acknowledgement),
  `arm_refused`, `hardware_busy`, `devices_absent`, `table_invalid`.
- **`random.start`** from `configured` arms first, with the same refusals. It then runs the
  system identification at `pretest.level_db` and the coherence gate, and either holds for review
  (`review`) or engages: the soft start, then the schedule. It answers at once (`running`).
- **`random.proceed`** releases the review hold and engages.
- **`random.level`**, in `manual` only: `delta_db` steps from the present level (the shell's
  plus and minus 1 dB), held between the first and last levels; `to_db` goes to a level of the
  schedule (`not_a_rung` otherwise). Every change ramps over `level_ramp_s`. Refusals:
  `manual_mode_required`, `not_a_rung`, `wrong_state`, `not_controller`.
- **`random.mode`** hands the ladder over live. `auto` to `manual` holds the present level;
  `manual` to `auto` resumes from the present level with its dwell restarted, and from a level
  between two of the schedule's, it moves to the one above when the dwell ends.
- **`random.stop`** is the controlled stop: while driving, the drive starts down at the next
  output block, not the next control loop, and ramps 40 dB down over `shutdown_ramp_s` (state
  `stopping`); the run ends `operator_stop` at the end of the control loop in which the ramp
  arrives. From `armed` or `review` it ends at once. It answers at once.
- **`random.abort`** ends the run within one control loop (`grid.loop_s`), without the shutdown
  ramp; the station takes the drive down with its own stop. The outcome is `operator_abort`. It
  answers when the loop has ended.
- `random.stop` and `random.abort` are accepted from any connection (see "Controller and
  Observers"); every other verb in this table needs control.
- `random.stop` and `random.abort` on a run that is not live answer with the state, unchanged.
- **`random.reset`** builds a new run from the same configuration (`configured`, a new
  `run_id`). It and `random.configure` are the ways out of `finished`.
- **`random.clear`** drops the configuration and releases the station (`unconfigured`).

### Shutting Down

The control service does not end its process with a run live: a killed process can leave an
output holding its last sample. Told to stop (Ctrl-C, SIGTERM or SIGHUP on Unix; Ctrl-C,
Ctrl-Break or a closed console on Windows; and, started with `--watch-stdin`, the end of its
standard input) it refuses every verb that could start or raise a drive (`shutting_down`; one
already under way finishes first), ends a live run with the controlled stop, the path of
`random.stop` (while driving, the level ramps down over `shutdown_ramp_s`; never an abort), and
exits once the run has ended. The run's outcome is `safe_stop`, and its message says the service
was shut down. Connections are served until then, so every screen sees the run end and keeps
Stop and Abort. A stop that has not ended well after its ramp (three control loops, ten seconds
and twice `shutdown_ramp_s`) is followed by the abort.

`--watch-stdin` is for a parent process that starts the service, such as the desktop app: the
parent stops it by closing the pipe on its standard input, never by killing it, and if the
parent dies the operating system closes the pipe, so the run stops the same way.

### Run State

`random.state` answers with the state object, and the `random.state` text event carries the same
object to every connection on every change of `state`, `phase`, `mode`, `level_db`, `alarm` or
`end`.

```
{"run_id":"r-0007","state":"finished","phase":"aborted","title":"Qualification Random",
 "source":"HARDWARE",
 "station":{"kind":"ni","class":"ni_real","devices":[{"id":"cDAQ1Mod1","serial":"01A2B3C4"}]},
 "grid":{"fs_hz":51200,"n_pts":8192,"df_hz":6.25,"lines":317,"loop_s":0.64,"actual":true},
 "clock":"shared","mode":"auto","levels_db":[-12,-6,0],"level_db":null,"full_level_s":300,
 "elapsed_s":131.2,"at_level_s":42.5,"remaining_at_level_s":null,"alarm":false,
 "feasibility":{"verdict":"not_evaluated","message":"No shaker was given, so the profile was not checked against shaker ratings."},
 "end":{"outcome":"aborted","message":"Envelope abort: 14 lines outside the abort band, worst +7.2 dB, at 0.0 dB.",
        "at_s":131.2,"level_db":0,
        "trip":{"kind":"envelope","abort_lines":14,"worst_db":7.2,"at_level_db":0,"at_loop":212}}}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured` | `idle` | No configuration |
| `configured` | `idle` | Configured; nothing open |
| `blocked` | `feasibility_blocked` | Feasibility exceeded; arming needs an acknowledgement |
| `armed` | `armed` | Station open and gated; nothing driven yet |
| `running` | `sysid`, `coherence_gate`, `engaged`, `full_level` | Driving: system identification, its gate, the ladder, full level |
| `review` | `awaiting_review` | System identification passed; nothing drives until `random.proceed` |
| `stopping` | `ramping_down` | A controlled stop or a safe stop is ramping the level down |
| `finished` | `stopped`, `aborted`, `coherence_hold` | Ended; `end` says how |

`phase` is the controller's own phase in snake case, with `armed` (armed, before the first
drive) and `ramping_down` added; `full_level` begins once the drive has reached full level (a
level change toward it reads `engaged` while it ramps). `run_id` is null when unconfigured, and new at every
configure and reset. `source`, `station`, `grid` and `clock` describe the run's station
(`clock`, `shared`, `independent` or `unknown`, is null until the run arms). `level_db` is the
commanded level (`pretest.level_db` during system identification, and the level the
ramp-down is headed for while `stopping`), null when nothing drives. `elapsed_s` counts from `random.start` (null
before). `at_level_s` and `remaining_at_level_s` are the time at the present level and the
time left on it (null in `manual`). `alarm` is true while an alarm condition holds (see
`random.live`). `feasibility` is null until arm, then `{"verdict","message"}` with `verdict`
`pass`, `advisory`, `exceeded` or `not_evaluated`.

`end` is null until the run finishes, then `{"outcome","message","at_s","level_db","trip"}`.
`level_db` and `message` give the level where the end began: where the drive was when the stop
or abort was asked for (a controlled stop's ramp-down starts there), or where the trip came;
`level_db` is null when nothing drove.

`record` is absent until the run arms, then `{"folder","streams","problem"}`: the run folder's
path, `streams` `on` or `off` (off when switched off in the configuration, or when the disk was
under its free-space floor at arm), and `problem`, null, or in words the latest thing the record
could not write (a stream stopped, a file not written). A problem never stops the run.

| `outcome` | How the run ended |
|---|---|
| `completed` | The schedule finished |
| `operator_stop` | `random.stop` |
| `operator_abort` | `random.abort` |
| `safe_stop` | The service stopped the run with the controlled stop: nobody held control for `timeout_s` (see Controller and Observers), or the service was shut down (see Shutting Down); `message` says which |
| `aborted` | A protection abort tripped; `trip` says which |
| `held` | The coherence gate held: the band coherence stayed below `coherence_floor`, so the run never drove at test level |
| `sysid_level_unreachable` | System identification could not bring the control response to `pretest.level_db` within its drive ceiling, or the response did not settle on it; the run never drove at test level |
| `fault` | The loop or the station failed; `message` is the controller's words |
| `law_fault` | The control law did not produce a usable drive for several loops in a row (3 by default); the run took its controlled stop, with its shutdown ramp. `trip` says why |

The shell shows every outcome but `completed` and `operator_stop` as an abort with `message`.
`trip` is null unless `outcome` is `aborted` or `law_fault`, and then has `at_level_db`, `at_loop`
and its `kind`'s facts. A trip during system identification has `at_loop` 0 and `at_level_db`
`pretest.level_db`, and `message` says it came during system identification:

| `kind` | Facts |
|---|---|
| `envelope` | `abort_lines`, `worst_db` |
| `kinematic` | `channel`, `axis` (`accel`, `velocity` or `displacement`), `value`, `limit` |
| `drive_vrms` | `realized_vrms`, `limit_vrms` |
| `divergence` | `consecutive_loops`, `divergent_bins` |
| `law_fault` | `reason`, `consecutive` (unusable law calls in a row) |

A `law_fault` trip's `reason` is what was wrong with the last unusable law output: `error` (the
law reported one), `panicked` (for a law loaded from a file: it trapped), `over_budget` (the call
took longer than its budget, by default a quarter of one loop's acquisition time), `wrong_length`,
`non_finite`, `negative` (a drive line), `non_finite_level`, or, for a law loaded from a file only,
`over_memory` (it asked for more memory than its cap) or `bad_diagnostics` (its diagnostics broke
the output rules). Its `outcome` is always `law_fault`, never `aborted`: the drive in force was
held through every miss, so the run stops the way an operator stop does.

### Live Display

Event topic `random.live`, a binary frame 5 times a second from the start of system
identification until the run finishes, and once more with the final state:

```
{"run_id","source","class","state","phase","mode","elapsed_s","level_db","commanded_db",
 "at_level_s","remaining_at_level_s","level_s","loop",
 "control_rms_g","reference_rms_g","control_error_db","drive_rms_v",
 "lines":{"in_band","alarm","abort"},
 "channels":[{"index":1,"description":"Control","role":"control","rms":6.02,"unit":"g","overload":false}],
 "alarms":[{"kind":"envelope","lines":2,"worst_db":3.4}],
 "views":[control spectrum, drive spectrum, run readings]}
```

- `level_db` is the level the drive is at now, read for every frame; after a level change it
  walks to the commanded level over `level_ramp_s`, and in a controlled stop it walks down over
  `shutdown_ramp_s`. Null when nothing drives.
- `commanded_db` is the level the run is headed for, the same value as the run state's
  `level_db` (`pretest.level_db` during system identification). While `level_db` and
  `commanded_db` differ, the drive is ramping. `phase` turns `full_level` only once the drive
  has reached full level, so a level change toward full level reads `engaged` while it ramps.
- `level_s` is the time the schedule holds the present level: its `dwell_s`, or `full_level_s`
  at full level. Null in `manual` and while nothing drives. With `at_level_s` and
  `remaining_at_level_s` it is a level's progress.
- `loop` counts control loops since the start. `control_rms_g` is the composite control rms
  over the band (null before the first loop), `reference_rms_g` the reference rms at the
  last control loop's level (the target), `control_error_db` the difference
  `20 log10(control_rms_g / reference_rms_g)` (null until both are known), and `drive_rms_v` the
  drive rms: the last loop's drive spectrum at the level the drive is at now. The Control view's reference and bands are drawn at the last
  control loop's level, the level its measured series belongs to.
- `lines` counts the band's lines, and how many lie outside the alarm and the abort bands in
  the last loop.
- `channels` lists every enabled input row in table order, with its rms over the band in its
  own unit and `overload` true once the row has reached 90 % of `range_max` this run, or the
  station has seen a sample at 98 % of the row's configured range. Every station makes that
  check in software, so it does not depend on the driver reporting overload (the NI 9234's
  driver does not).
- `alarms` lists the conditions holding now: `{"kind":"envelope","lines","worst_db"}` (lines
  outside the alarm band) and `{"kind":"overload","channel"}`. An alarm warns; it does not stop
  the run.

The views are the viz kit's, in this order:

1. **Control** (`spectrum`, x `Frequency` Hz log, y `PSD` g²/Hz log, `link` `frequency`), series
   in this order: `Control` (`measured`; absent until the first loop), `Reference`
   (`reference`), `Alarm high` and `Alarm low` (`tolerance`), `Abort high` and `Abort low`
   (`abort`). Every series is on the band's lines of the grid the run actually uses, with
   `shape: "steps"` and its `edges`, so each line is drawn flat across its own bin (the
   stairstep). The reference and the bands are drawn at the last control loop's level, with
   each row's own bands applied.
2. **Drive** (`spectrum`, y `PSD` V²/Hz log, same `link`): `Drive` (`drive`), stepped on the same
   lines.
3. **Run** (`reading`): the numbers above as rows, for a pane that shows readings.

`random.live` is the **default subscription**: what every connection receives until it asks for
something else (below).

### Live Data Subscriptions

A screen asks for the quantities and channels its plots show, at the rate it wants, and the
service sends that connection only those. On a large channel count this is what keeps a display
feed a display feed: a channel nobody plots is not computed into a frame or sent. A subscription
narrows the display and nothing else; the control loop reads every channel at the station's
rate whatever any screen asks for.

| Method | Params | Result |
|---|---|---|
| `random.subscribe` | `{"rate_hz","plots":[plot...]}`, or `{"default":true}` | `{"default","rate_hz","plots","quantities"}` |

```
{"rate_hz":5,"plots":[{"id":"ctl","quantity":"control"},
                      {"id":"lim","quantity":"response","channels":"limited"},
                      {"id":"t2","quantity":"time","channels":[2,3],"points":1024}]}
```

- Every connection starts on the default subscription and receives `random.live`. A
  `random.subscribe` with `plots` replaces it: from then on the connection receives `random.data`
  (below) at `rate_hz` and no `random.live`. `{"default":true}` returns it to `random.live`.
- Each call replaces the connection's subscription as a whole; a client sends its full list
  whenever a plot is added, removed or changed. The subscription belongs to the connection, lasts
  until it closes, and carries over from run to run, so a screen subscribes once and again only
  when its plots change.
- It only reads, so it is open to every connection, controller or observer (a window on another
  monitor is an observer with a subscription of its own), in any run state, configured or not.
- `rate_hz` is 0.5 to 10, default 5: the most frames per second the connection is sent.
- `plots` holds up to 64 entries, and may be empty (the run's numbers only, for a window that
  shows no plot). Each is `{"id","quantity","channels","points"}`. `id` is the client's own name
  for the plot, 1 to 64 characters and unique in the list; the service returns it with the
  plot's data. `quantity` is one of the table below. `channels` applies to `response` and `time`
  only: a list of channel indexes, or `"all"` (every enabled input, the default), `"control"`
  (the control rows) or `"limited"` (the rows the configuration limits). The service resolves it
  against the run's table on every frame, in table order; an index the run does not have is left
  out, never refused, so a plot kept across a table edit shows fewer channels rather than
  failing. `points` applies to `time` only: the most points sent per channel, 64 to 8192,
  default 1024.
- Refusals: `bad_params` with `param` naming the field (for example `plots[2].quantity`): an
  unknown quantity, `channels` or `points` on a quantity they do not apply to, a duplicate `id`,
  a value out of range.
- The result echoes the subscription with every default filled in (`default` false, `plots` as
  given), or `{"default":true,"rate_hz":5,"plots":null}`, and `quantities`, the list of quantities
  this service offers, so a screen offers only what the service can send.
- When the run has already streamed, the service also sends the connection one `random.data`
  frame at once, so a plot added mid-run, or a window opened after the run ended, fills without
  waiting.

| `quantity` | Views, in order | What they show |
|---|---|---|
| `control` | one `spectrum` | The **Control** view of `random.live`: control against the reference, with the alarm and abort lines |
| `drive` | one `spectrum` | The **Drive** view of `random.live` |
| `response` | one `spectrum` per resolved channel | That channel's PSD over the band, titled `Channel <index>: <description>` (the role when there is no description), y `PSD` in the channel's unit squared per Hz: a series `Channel <index>` (`measured`, stepped on the run's lines; absent until the first loop), and when the configuration limits the channel, a series `Limit` (`limit`, stepped) on the lines its rows cover, each across the same bin as the measured line |
| `time` | one `spectrum` per resolved channel | The last control loop's acquired samples of that channel, same title, x `Time` s linear, counted from the start of the loop's segment, y `Amplitude` in the channel's unit, linear: a series `Channel <index>` (`measured`, shape `line`; absent until the first loop). Decimated to `points` as a minimum and a maximum per interval, in time order, so no peak is lost |
| `coherence` | one `spectrum` | The system identification's ordinary coherence between drive and control on the band's lines, x `Frequency` Hz log, y `Coherence` linear (0 to 1), `measured`, shape `line`. No series until identification has measured it |
| `readings` | one `reading` | The **Run** view of `random.live` |

Event topic `random.data`, a binary frame to one connection, at most `rate_hz` times a second
from the start of system identification until the run finishes, and once more with the final
state. It carries every field of `random.live` except `views`, then `plots`, in the
subscription's order:

```
{..., "plots":[{"id":"lim","quantity":"response","channels":[2,3],"views":[spectrum, spectrum]}]}
```

`channels` is the resolved list (null for the quantities without channels), and for `response`
and `time` the views are in that order, one per channel. A plot whose channels resolve to none
has `channels: []` and no views.

### Run Log

`random.log` returns `{"entries":[entry...],"next"}` from `since`, and the `random.log` text
event carries `{"run_id","entries":[...]}` as entries are written. An entry is
`{"seq","at_unix","kind","text"}`, `kind` one of `state`, `feasibility`, `override`, `station`,
`gate`, `level`, `alarm`, `overload`, `trip`, `note`, `record` (the run's record could not write
something), and for classical shock also `pulse` (one
line per pulse fired) and `law` (the law's calls that were not used). The log starts at configure
and is the run's level history and alarm record. `shock.log` and its event take the same form.

### Run Folders

Every Random run and every classical shock run that arms leaves a run folder in the service's data folder, laid out in
`docs/run-folder.md`, which the analysis side converts into a project file
(`hertzforge.io.run_convert`). It is claimed at arm, never written over (a taken name gets the
next free one) and valid at every moment: `run.json`, `table.json` (the table the run armed
with, every row as `channels.get` gives it, with `revision` and `sha256`) and `setup.json` (the
configuration as accepted, and what it resolved to) are written before the first sample, or the
arm is refused `record_unwritable`. While the run goes, every run log entry is appended to its
`log.jsonl` as it is made, with the station frame where one applies and structured `data` (a
level change's level, an alarm's lines, a trip's facts, an overload's channel, who acted), and
the inputs and the drive are recorded as streams unless `record.streams` is false. At the end,
any way the run ends, `end.json` holds `end` as `random.state` reports it, and `run.json` says
`complete`. A service stopped mid-run leaves the folder with `complete` false. The state's
`record` names the folder from arm on. A classical shock run's streams are runs of shots, one for
its identification and one for each pulse fired, each named in the stream's `shots`, and its
results include each pulse's SRS, verdicts and kinematics, written as the pulse is judged
(`docs/run-folder.md`, "What a Classical Shock Run Saves"). `shock.state` carries `record` as
`random.state` does. An SRS shock run's record is laid out the same way: shots for its
identification and every pulse, and results for the target with its envelope, the synthesis, the
identification and each pulse's SRS, drive table, kinematics and verdict (`docs/run-folder.md`,
"What an SRS Run Saves"); `srs.state` carries `record` as `random.state` does. A TWR run's streams
are shots too, one for its identification and one for each play, the play the run ended on and one
cut short included, and its results include each play's measures and outcome and every call of its
law (`docs/run-folder.md`, "What a TWR Run Saves"); `twr.state` carries `record` likewise.

The record is written on its own thread. A failure to write it is a log entry of kind `record`
and the state's `record.problem`; the run goes on.

## MIMO Random

Multiple-input multiple-output Random: up to 24 drives and 72 control channels, controlled to a
reference spectral density matrix. The service runs one test at a time, so a MIMO run and a
single-output Random run (or a `sim.start` run, or a classical shock, SRS shock, Sine, TWR, Explorer or
Recorder run)
exclude each other exactly as two Random runs would: each is refused `run_active` while the other
is live. The controller and observer rules
are Random's: **any connected screen may stop or abort** (`mimo_random.stop`,
`mimo_random.abort`); `mimo_random.setup` (other than a dry run), `start`, `proceed`,
`override`, `exclude` and `iterate_prediction` need control; everything that only reads is open to every
connection.

**Conventions.** A spectral matrix is `G = lim (1/T) E[X X^H]` per line, so a response `Y = H X`
has `G_yy = H G_xx H^H`. This is the complex conjugate of the Bendat and Piersol definition; a
client converting from that convention conjugates every cross spectrum. A Hermitian matrix travels
packed: per line its upper triangle row by row (`G00, G01, ..., G0n, G11, ...`), lines in order,
the element `(r, c)` with `r <= c` at `r (2n - r + 1) / 2 + (c - r)` within its line. Real and
imaginary parts travel apart. Controls and drives are counted from 0 in the setup's order; that is
what every index on the wire counts.

| Method | Params | From | Result |
|---|---|---|---|
| `mimo_random.setup` | `{"setup","dry_run"}` (`dry_run` optional) | any but live | `{"state","validation"}` |
| `mimo_random.validate` | `{"setup"}` (optional) | any | `{"validation"}` |
| `mimo_random.start` | `{}` | `configured` | status |
| `mimo_random.proceed` | `{}` | `review`, `awaiting_review` | status |
| `mimo_random.override` | `{"ack","reason"}` (`reason` optional) | `review`, `gate_failed` or `prediction_exceeded` | status |
| `mimo_random.exclude` | `{"controls","ack"}` | `review`, `gate_failed` | status |
| `mimo_random.iterate_prediction` | `{"law"}` (optional) | `review` | `{"prediction"}` |
| `mimo_random.stop` | `{}` | any | status |
| `mimo_random.abort` | `{}` | any | status |
| `mimo_random.status` | `{}` | any | status |
| `mimo_random.report` | `{}` | any | `{"report"}` |
| `mimo_random.subscribe` | see "MIMO Random Display" | any | as there |
| `mimo_random.identification.save` | `{"path"}` | after identification | `{"identification"}` |
| `mimo_random.identification.load` | `{"path"}` | `configured` | `{"identification"}` |
| `mimo_random.reference.build_breakpoints` | see "MIMO Random Reference" | any | the reference result |
| `mimo_random.reference.build_rigid_body` | as there | any | the rigid-body result |
| `mimo_random.reference.validate` | as there | any | the reference result |
| `mimo_random.reference.import` | as there | any | the reference result |

"status" is the `mimo_random.status` object below.

An identification set (the FRF, coherences, conditioning and spectral matrices measured during
identification, with the setup hash they were measured on) can be kept and used again.
`mimo_random.identification.save` copies the present run's set into the folder `path` (any
time after identification, the finished run included). `mimo_random.identification.load` names
a saved set before `mimo_random.start`: it is checked against the setup and the channel table as
they stand (the channels, their sensitivities, the transformations and the grid), refused
`identification_mismatch` when they differ, and the start that follows skips the noise floor and
identification and goes to the gates. `{"identification"}` is
`{"path","setup_hash","frames","level_db","drive_vrms"}`. Both are the controller's.

### MIMO Random Setup

```
{"title":"Two-Axis Random",
 "channels":{"controls":[{"channel":1,"alarm_db":[3,3],"abort_db":[6,6]},{"channel":2}],
             "drives":[{"channel":3,"ceiling_vrms":2},{"channel":4,"ceiling_vrms":2,"abort_vrms":3}]},
 "space":{"kind":"physical"},
 "reference":{"kind":"inline","freqs_hz":[100,106.25,...],"n":2,"re":[...],"im":[...]},
 "law":{"id":"pinv_closed","params":{"convergence":0.5}},
 "estimation":{"fs_hz":8192,"n_pts":4096,"frames_per_loop":8,"window":"hann","overlap":0.5,"memory_loops":4},
 "identification":{"level_db":-20,"frames":32,"noise_frames":16,"coherence_floor":0.5,
                   "condition_limit":1000,"review":true},
 "control":{"frf":"hold","level_ramp_s":3,"shutdown_ramp_s":3,"max_loops_out":5},
 "schedule":{"mode":"auto","levels":[{"db":-6,"dwell_s":10},{"db":0}],"full_level_s":60},
 "backend":{"kind":"sim"}}
```

| Field | Default | Meaning |
|---|---|---|
| `channels.controls` | (required) | The control channels, by their table `index` (each an enabled `control` row), each with its own `alarm_db` and `abort_db` bands (`[below, above]` in positive dB, defaults `[3,3]` and `[6,6]`) |
| `channels.drives` | (required) | The drives, by table `index` (each an enabled `drive` row), each with its own `ceiling_vrms` (above 0, at most 10) and optional `abort_vrms` (above the ceiling), `peak_v` (the clip level, at least the ceiling and at most 10; default 3 times the ceiling, at most 10), `shaker` (the shaker record, as Random's feasibility takes it; absent: feasibility not evaluated for that drive) and `moving_mass_lb` (that shaker's own moving mass, default 0) |
| `channels.limits` | none | Monitored responses the core holds under a ceiling: each an enabled `monitor` row by `index`, with its PSD `ceiling` (above 0, the unit squared per hertz, at full level) and `abort_margin_db` (above 0, default 3): a measured PSD that far above the ceiling aborts. `limit_unknown_channel` otherwise |
| `channels.article_mass_lb` | 0 | The test article's mass, split equally across the shakers for feasibility |
| `space` | physical | `{"kind":"physical"}`: control the channels themselves. `{"kind":"virtual","dofs":[names],"response_transform","drive_transform","dof_bands"}`: control the named degrees of freedom; `response_transform` is degrees of freedom by control channels, `drive_transform` (optional) drives by virtual drives. A transformation is `{"rows","cols","values"}`, real, row by row. `dof_bands` (optional) gives one `{"alarm_db","abort_db"}` per degree of freedom, in their order (defaults `[3,3]` and `[6,6]`): under virtual control the reference and its tolerances are in the control space, so the bands are the degrees of freedom's and the control rows' own bands are not used for tolerance |
| `reference` | (required) | Inline, for a small reference: `{"kind":"inline","freqs_hz","n","re","im"}`, the packed matrices of `n` channels (or degrees of freedom) on the lines `freqs_hz` (at least 8, rising), at most 262,144 values; lines off the run's grid are interpolated onto it, each packed element linearly between the two reference lines around a grid line (a mix of two positive semi-definite matrices stays one), so a programmatic client need not build on the grid. By file, for a large one (which must already be on the run's grid; a file off the grid is refused): `{"kind":"file","path","sha256"}`, a native reference file on the computer that runs the test, named with the SHA-256 of its bytes; the run reads it when it starts and refuses a file whose hash differs |
| `law` | `{"id":"pinv_closed"}` | The control law, by its catalog `id` (see "Control Laws"), and values for the parameters its manifest declares |
| `estimation` | as shown | The frame (`fs_hz` 256 to 204,800, `n_pts` a power of two from 64), frames per loop (1 to 1024), window (`hann` or `rectangular`), overlap (0 to 0.75) and how many loops the estimate remembers (1 to 64) |
| `identification` | as shown | The identification level (-60 to 0 dB relative to the reference), its frames and the noise-floor frames, the gates (the smallest multiple coherence over the band, `coherence_floor`, and the largest FRF condition number, `condition_limit`), and whether to hold for review afterwards |
| `control` | as shown | `frf` `hold` (the FRF from identification) or `update` (re-estimated each loop); ramp times; consecutive loops out of tolerance before the divergence abort; `frf_blend` (under `update`, the weight of each loop's estimate against identification's, above 0 and at most 1, default 0.5); `cap_up_db` and `cap_down_db` (the core's per-loop rise and fall caps on each drive's PSD, default 3.5 and 6 dB, the rise at most 6); `law_budget` (the law call's time budget, a fraction of the loop, default 0.25) and `law_misses` (unusable law calls in a row that stop the run, 1 to 100, default 3); `soft_start_db` (single-output Random's soft start: the loop engages this many dB below the lowest scheduled level and ramps to the first rung over the level ramp, or 5 s when that is 0, with the envelope's bands widened by 6 dB until it arrives; the first rung's dwell starts there; 0 to 30, default 9, 0 for none) |
| `schedule` | (required) | Single-output Random's schedule |
| `backend` | the selected station | `{"kind":"sim"}` or `{"kind":"ni"}`; must be the station selected (`backend_not_selected` otherwise) |
| `operator`, `article`, `notes` | empty | The operator's words for the run's record (`docs/run-folder.md`) |
| `specification` | none | `{"document","clause"}`: the test document the run answers, for its record |
| `record` | `{"streams":true}` | `streams` false: the run's record keeps no time histories |

A setup whose shape is wrong (a missing or mistyped field) is refused `bad_params` naming the
field. Everything about its content is in the validation report, so the operator sees every
problem at once: `mimo_random.setup` with errors is refused `setup_invalid` with the report as
`detail`, and nothing is replaced. `dry_run: true` returns `{"validation"}` only, needs no
control and replaces nothing; `mimo_random.validate` does the same for the setup it is given, or
returns the present setup's report (null when there is none). A setup replaces the one before
and is refused `run_active` while any run is live. An accepted setup's report is also sent to
every connection as the `mimo_random.validate` text event (`{"run_id","validation"}`).

### MIMO Random Validation

```
{"ok":true,"errors":[],"warnings":[],
 "shape":{"controls":2,"drives":2,"space":"physical","dofs":null,"shape":"square"},
 "transforms":{"response":null,"drive":null},
 "reference":{"source":"inline","lines":8,"n":2,"band_hz":[100,143.75],"min_eigenvalue_ratio":0,
              "rank_min":2,"rank_max":2,"repaired_lines":0},
 "feasibility":[{"channel":3,"ceiling_vrms":2,"verdict":"not_evaluated","predicted_vrms":null,"message":"..."}],
 "law":{"id":"pinv_closed","name":"pseudo-inverse, closed loop","version":"1.0.0","sha256":null,"admitted":true}}
```

- `ok` is true exactly when `errors` is empty. Items are the table's `{"reason","message"}` with
  `channel` or `field`.
- `shape.shape` is `square`, `more_controls_than_drives` or `fewer_controls_than_drives`
  (controlled quantities against drives).
- Each transformation reports its `rank` and `condition` (largest over smallest singular value;
  null when rank-deficient). Rank below `min(rows, cols)` is an error; a condition number above
  100 is a warning.
- The reference reports, over its lines, the most negative eigenvalue relative to the line's
  largest (`min_eigenvalue_ratio`, 0 when none is negative), the smallest and largest rank, and
  how many lines the run will repair. An eigenvalue below -1e-3 of its line's largest is an
  error (no signal has those spectra); one between -1e-3 and -1e-6 is repaired at the run
  (clamped to zero, each PSD kept) and warned. A rank above the number of drives is a warning:
  the reference can then be met only in the least-squares sense. A reference by file is read
  when the run starts, so its fields are null until then.
- `feasibility` lists every drive: `verdict` `pass`, `advisory`, `exceeded` or `not_evaluated`.
  Before identification the drive a test needs is not known, so a setup's report says
  `not_evaluated`; the prediction (below) gives the numbers before the loop engages.
- `law.admitted` is false when the law is unknown, single-output, does not support the test's
  shape, or a parameter is unknown or out of its declared range.

### MIMO Random Run State

`mimo_random.status` answers with the status object, and the `mimo_random.status` text event
carries the same object to every connection on every change of state, phase, level, loop,
alarm, faults or prediction.

```
{"run_id":"m-0003","state":"running","phase":"engaged","title":"Two-Axis Random","source":"SIM",
 "station":{"kind":"sim","class":"sim","devices":[]},
 "grid":{"fs_hz":8192,"n_pts":4096,"df_hz":2,"lines":8,"loop_s":2.25,"actual":false},"clock":null,
 "shape":{"controls":2,"drives":2,"space":"physical","dofs":null,"shape":"square",
          "labels":["Control X","Control Y"],"drive_labels":["Shaker X","Shaker Y"]},
 "mode":"auto","levels_db":[-6,0],"level_db":-6,"elapsed_s":31.5,"at_level_s":4.5,
 "remaining_at_level_s":5.5,"loop":14,"alarm":false,"control_error_db":[0.42,-0.31],
 "gates":{"coherence_min":0.962,"coherence_floor":0.5,"condition_max":12.5,"condition_limit":1000,"passed":true},
 "caps":[1,0.875],"drive_rms_v":[0.412,0.388],
 "law":{"id":"pinv_closed","name":"pseudo-inverse, closed loop","version":"1.0.0","sha256":null},
 "faults":[],"override":null,"prediction":{...},"end":null,
 "record":{"folder":"/home/operator/Hertzforge_Data/20261113-140000_mimo_random_m-0003",
           "streams":"on","problem":null}}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured` | `idle` | No setup |
| `configured` | `idle` | Set up; nothing open |
| `armed` | `armed` | Station open; nothing driven yet |
| `running` | `sysid_noise`, `sysid`, `engaged`, `full_level` | Driving: the noise floor with the drives off, identification, the ladder, full level |
| `review` | `awaiting_review`, `gate_failed`, `prediction_exceeded` | Identification is done; nothing drives until `proceed` (gates passed, prediction within every ceiling) or an `override` of what holds the run (a failed gate, or a prediction over a ceiling) |
| `stopping` | `ramping_down` | A controlled stop or a safe stop is ramping the level down |
| `finished` | `stopped`, `aborted` | Ended; `end` says how (Random's outcomes and trips) |

`labels` and `drive_labels` name what the indexes count: the controlled quantities (control
channels, or degrees of freedom under virtual control) and the drives. `control_error_db` is
each controlled quantity's rms over the band against the reference's, in dB (empty before the
first loop). `gates` is null until identification has measured them. `caps` is each drive's
cap factor this loop (1 when the core did not hold it down). `prediction`, once identification
is done: `{"iteration","law","drive_rms_v","ceiling_vrms","error_db","drives_pass",
"has_achievable","unreachable_db","drive_accel","accel_control","feasibility"}`, what the law
would ask of each drive and leave on each control. `has_achievable` is true when controls
outnumber the drives, so only the achievable reference `P G_ref P^H` (`P = H H^+`) can be reached;
`unreachable_db` is each controlled quantity's achievable rms against the reference's over the
band (0 where all of it is reachable), and the whole achievable matrix is the `pred_sdm`
quantity. `drive_accel` is each drive's predicted rms acceleration at its shaker at full level,
read at the physical control that drive moves most (`accel_control`, a control position), from
the identification FRF and the predicted drive; `feasibility` is each drive's shaker against that
acceleration (`{"verdict","message"}`, `verdict` `pass`, `advisory`, `exceeded` or
`not_evaluated`). `override` is the latest acknowledgement, `{"by","text","reason"}`, and
`overrides` lists every one in order. `shape.bands` gives each controlled quantity's
`{"alarm_db","abort_db"}`: its control row's, or under virtual control its degree of freedom's.
`excluded` lists the control positions taken out of control at the gate
(`mimo_random.exclude`). `faults` lists `{"kind","message"}`.

- **`mimo_random.start`** arms the station and runs identification, then holds for review when
  `identification.review` is true or a gate failed; otherwise it engages. It answers at once. Every
  run writes its run folder from arm (`docs/run-folder.md`, "What a MIMO Random Run Saves"); the
  start is refused `record_unwritable`, with the path and the reason in `detail`, when the record
  cannot be written, and nothing has been driven then. `record` in the status is
  `{"folder","streams","problem"}` from arm on (null before): the run's folder, `on` or `off`, and
  the latest problem writing it, in words, or null. A problem never stops the run.
- **`mimo_random.proceed`** engages from `awaiting_review`. After a failed gate it is refused
  `gate_failed`.
- **`mimo_random.override`** goes on past what holds the run, in the operator's own words: a
  failed identification gate (`reason` `gate_failed`), or a prediction that asks a drive for more
  than its ceiling (`reason` `prediction_exceeded`, the single-output feasibility pattern: the
  ceilings still hold every drive during the run). One acknowledgement passes one hold: after a
  gate override the run goes on to the prediction, and engages only when the prediction is within
  every ceiling; otherwise it holds again at `prediction_exceeded` for its own acknowledgement.
  `reason` absent means whatever holds the run now; a `reason` that names something else is
  `override_not_needed` and says what holds the run. `ack` is kept with the run and in its
  report; an empty one is `bad_params`, and an override when nothing holds the run is
  `override_not_needed`.
- **`mimo_random.exclude`** takes the controls at the listed positions out of control at a
  failed gate (physical control only), in the operator's words (`ack`): each is still measured
  and recorded, as a monitor; the reference and its tolerances are reduced to the controls kept,
  and identification's coherence and conditioning are judged again on them, so the run goes on
  to the prediction when the gate now passes and holds again when it does not. Positions keep
  counting the setup's controls: an excluded control is listed in the status's `excluded`, its
  entries in the per-control lists are null, its curves are not sent and its heatmap row is
  empty. Refusals: `wrong_state` (no failed gate, or under virtual control, where the response
  transformation is changed in the setup instead), `bad_params` (an empty reason, a position the
  setup does not have, one already excluded).
- **`mimo_random.iterate_prediction`** recomputes the prediction during the review hold, with
  another law or other parameters (`{"law":{"id","params"}}`, checked like the setup's) or the
  run's own. A law chosen here is the one that engages.
- **`mimo_random.stop`** and **`mimo_random.abort`** behave as Random's; the safe stop when
  nobody holds control is the same.

### MIMO Random Report

`mimo_random.report` returns `{"report"}`, null until the run has finished; the
`mimo_random.report` text event carries `{"report"}` to every connection when it finishes.

```
{"run_id","title","source","grid","shape","law","loops",
 "controls":[{"label","rms","reference_rms","error_db"}],
 "drives":[{"channel","label","rms_v","ceiling_vrms","min_cap"}],
 "gates","override","end"}
```

### MIMO Random Reference

The reference is built, read and checked by the service, so a client shows what the controller
computes and repeats none of its math. Every method here only computes or reads; none needs
control, and none replaces a setup. A reference small enough for a setup (at most 262,144 packed
values, at least 8 lines) comes back inline, ready to send as the setup's `reference`; a larger
one is written once into the data folder as a reference folder named by its content hash and
comes back as `{"kind":"file","path","sha256"}`.

| Method | Params |
|---|---|
| `mimo_random.reference.build_breakpoints` | `{"grid":{"fs_hz","n_pts"},"unit","tables":[{"label","rows":[breakpoint...]}],"pairs":[{"row","col","coherence","phase_deg"}],"unspecified","alarm_db","abort_db","repair","preview_pair"}` |
| `mimo_random.reference.build_rigid_body` | `{"sensors":[{"label","position","direction"}],"reference_point","unit","length_unit"}` |
| `mimo_random.reference.validate` | `{"reference","labels","unit","preview_pair"}`: an inline reference or a reference folder |
| `mimo_random.reference.import` | `{"path","pair_path","grid","labels","unit","repair","preview_pair"}` |

- `grid` is the test's frame; the lines run every `fs_hz / n_pts` hertz up to `fs_hz / 2.56`.
- `tables` hold one PSD breakpoint table per controlled quantity (control channel or degree of
  freedom), in order, with Random's breakpoint rows. A pair's `coherence` (squared, 0 to 1) and
  `phase_deg` (of `G_row,col`) are each a number or `[[frequency, value], ...]` joined by straight
  lines and held beyond the ends. Pairs not listed are `independent` (coherence 0) or
  `fully_coherent` (coherence 1, phase 0). `preview_pair` (default `[0,1]`) is the pair the
  preview draws.
- A line whose coherences and phases cannot all hold (the matrix has a negative eigenvalue) is
  an error, `reference_not_psd`, and the result describes the repair: each negative eigenvalue
  set to zero and the result scaled back to the PSDs entered. With `repair: true` the repaired
  reference is accepted (warned `reference_repaired`).
- `import` reads, by full path on the computer that runs the test, a reference folder, an `.npz`
  specification (needs `grid`; cross terms are conjugated on the way in) or the CSV layout (a
  PSD table, and a pair table at `pair_path`; needs `grid`). A `.mat` or `.hforge`
  specification is converted to a reference folder first. A relative path is `bad_params`;
  anything else that fails is an error item, `reference_file`.

The reference result:

```
{"ok":false,"reference":null,"labels":["Front","Rear","Side"],"unit":"g","df_hz":8,
 "band_hz":[24,496],"lines":60,"errors":[{"reason":"reference_not_psd","field":"reference","message":"..."}],
 "warnings":[],
 "psd":{"violations":60,"min_eigenvalue_ratio":-0.46,"worst_hz":24,"lines_hz":[24,32,...],
        "repair":{"lines":60,"clamped":60,"max_change":0.37,
                  "pairs":[{"row":1,"col":2,"labels":["Rear","Side"],"before":0.9,"after":0.53,"freq_hz":24}]},
        "repair_accepted":false},
 "transforms":{"response":null,"drive":null},"provenance":null,"views":[...]}
```

`ok` is true exactly when a reference was accepted. `psd.min_eigenvalue_ratio` is the most
negative eigenvalue of the matrix as entered relative to its line's largest. `transforms` carry a
response or drive transformation the reference brings (`{"rows","cols","values","row_labels",
"col_labels","condition"}`), and `provenance` how it was made. `views` are four `spectrum` views
on the band lines, reduced to at most 512 points (each point the chunk's extreme): every PSD on
one plot, the smallest eigenvalue per line against the refusal threshold, and the preview pair's
coherence and phase (as entered and, when a repair was accepted, after it). A result with views
travels as a binary frame.

The rigid-body result: `{"ok","errors","warnings","dofs","dof_units","transform","condition",
"singular_values","sensors":[{"label","check_share"}]}`. `transform` is the response
transformation onto `X, Y, Z, RX, RY, RZ` at the reference point (6 by the sensors); fewer than six
sensors, or a geometry that resolves fewer than six degrees of freedom, is an error item
`geometry` naming the rank. `check_share` is how much of each sensor's reading the others can
check (the residual projector's diagonal, 0 to 1); a sensor at 0 is the only one in its
direction, and a departure there from rigid motion cannot be seen.

### MIMO Random Display

A screen asks for the quantities its plots show, by name and selector, at the rate it wants, and
the service sends that connection only those, each decimated to its plot's width. Nothing is sent
that is not on screen: a 72 by 72 spectral matrix is never streamed whole. The mechanics are
Random's: a subscription replaces the connection's last as a whole, belongs to the connection and
carries over from run to run, is open to every connection, and is answered with one frame at
once when the run has streamed.

| Method | Params | Result |
|---|---|---|
| `mimo_random.subscribe` | `{"rate_hz","plots":[plot...]}`, or `{"default":true}` | `{"default","rate_hz","plots","quantities"}` |

```
{"rate_hz":10,"plots":[{"id":"c01","quantity":"coh","pair":[0,1],"points":512},
                       {"id":"heat","quantity":"error_db","rows":"all","points":512},
                       {"id":"grid","quantity":"sdm","block":[0,1,2,3,4,5,6,7],"points":256}]}
```

- With no subscription (the default) a connection receives `mimo_random.status` only.
- `rate_hz` is 0.5 to 10 (default 5); `plots` holds up to 64 entries, each
  `{"id","quantity",<selector>,"points"}`. `points` is the plot's width in pixels, 64 to 8192
  (default 1024): the service sends at most that many points per series.
- A selector is one of `index` (a control or drive), `pair` (two of them), `rows` (`"all"` or a
  list of controls: the heatmap's rows), `diagonal: true` (every control or drive), `block`
  (1 to 8 controls: a block of the spectral matrix, the grid's window) or `key` (a law
  diagnostic, with an `index` for a per-control or per-drive layout). Positions count the setup's
  controlled quantities and drives from 0, in order; the status's `shape.labels` and
  `shape.drive_labels` name them. A position the run does not have is left out (the plot has no
  views), never refused.
- Refusals: `bad_params` with `param` naming the field: an unknown quantity, a selector the
  quantity does not take or two selectors, a pair of one channel where two are needed, a block
  over 8 or with a repeat, a value out of range, a duplicate `id`.

| `quantity` | Selector | Views |
|---|---|---|
| `psd` | `index` or `diagonal` | one `spectrum`, stepped: the control's PSD against the reference and its alarm and abort lines (`index`), or every control's PSD (`diagonal`) |
| `coh` | `pair` | one `spectrum`: the coherence of two controls against the reference's, 0 to 1 |
| `phase` | `pair` | one `spectrum`: the phase of `G_ij`, degrees, against the reference's |
| `csd_mag` | `pair` | one `spectrum`, stepped: the cross-spectrum magnitude against the reference's |
| `error_db` | `rows` | one `heatmap`: each control's dB error against the reference per line |
| `coh_error` | `rows` | one `heatmap`: each control's coherence error per line, the rms over its partners of the measured squared coherence minus the reference's (0 to 1; 0 with one control) |
| `drive_psd` | `index` or `diagonal` | one `spectrum`, stepped, V²/Hz |
| `drive_coh` | `pair` | one `spectrum`: the coherence of two drives |
| `frf` | `pair` (`[control, drive]`) | two `spectrum`s: the FRF's magnitude and phase |
| `sv` | none | one `spectrum`: the FRF's singular values per line |
| `cond` | none | one `spectrum`: the FRF's condition number per line, with the gate as a `limit` |
| `mcoh` | `index` or `diagonal` | one `spectrum`: a control's multiple coherence with the drives |
| `cap` | `index` or `diagonal` (default) | one `reading`: each drive's cap factor this loop |
| `dof_rms` | none | one `reading`: each controlled quantity's rms over the band |
| `trace` | none | one `spectrum`, stepped: the sum of the control PSDs against the reference's |
| `pred_drive_rms` | `index` or `diagonal` (default) | one `reading`: each drive's predicted rms against its ceiling |
| `pred_error_db` | `index` | one `spectrum`: a control's predicted error per line, dB |
| `law_diag` | `key` (and `index`) | one `spectrum` for a per-line layout, one `reading` for a scalar |
| `sdm` | `block` | one `sdm`: the block of the response matrix and the reference's |
| `pred_sdm` | `block` | one `sdm`: the block of the achievable reference `P G_ref P^H` at full level (as `measured`) and the reference's, after identification, when controls outnumber drives |
| `pred_drive_accel` | `index` or `diagonal` (default) | one `reading`: each drive's predicted acceleration at its shaker, marked by its shaker's verdict |
| `readings` | none | one `reading`: the run's numbers (loop, level, largest error, gates, elapsed) |

**Decimation.** The service reduces every quantity to the plot's width before it is framed:
a spectrum to one point per pixel column (columns spaced on the plot's frequency axis), each the
minimum and maximum of the lines in it (`y_min` and `y`, with the column's `edges` for a stepped
series), so every original line lies inside its column's envelope; a heatmap row and a matrix
block to the line nearest each column's center; readings not at all. When the run has no more
lines than the plot has columns, nothing is decimated and each line is sent as it is.

Event topic `mimo_random.data`, a binary frame to one connection, at most `rate_hz` times a
second from the start of identification until the run finishes, and once more at the end:

```
{"run_id","source","state","phase","loop","elapsed_s","level_db",
 "plots":[{"id","quantity","selector":{"pair":[0,1]},"labels":["Control X","Control Y"],"views":[...]}]}
```

`selector` echoes the plot's selector; `labels` names what it resolved to (the two channels of a
pair, the rows of a heatmap, the channels of a block).

## Classical Shock

A classical shock test fires one pulse at a time and closes the loop between pulses. The service
identifies the system with band-limited random at a stated drive in volts, inverts the identified
plant, and seeds the control law from it; after each pulse is fired, measured and judged, the law
proposes the next full-level drive and the service checks it before it plays. The drive is in
volts throughout. The run is one test like any other: a classical shock run, a Random run, a MIMO
run, an SRS shock run, a Sine run, a TWR run, an Explorer run, a Recorder run and a `sim.start` run
exclude each other, each refused `run_active` while another is live.
**Any connected screen may stop or abort** (`shock.stop`, `shock.abort`); `shock.configure`
(other than a dry run), `arm`, `start`, `proceed`, `fire`, `level`, `train`, `burst`, `reset`
and `clear` need control; everything that only reads is open to every connection. While the
service is shutting down, `shock.arm`, `start`, `proceed`, `fire`, `level`, `train` and `burst`
are refused `shutting_down`.

| Method | Params | From | Result |
|---|---|---|---|
| `shock.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived","views"}` |
| `shock.config` | `{}` | any | `{"run_id","config","derived","views"}` |
| `shock.preview` | `{"pulse","standard","fs_hz"}` (`standard` and `fs_hz` optional) | any | `{"derived","views"}` |
| `shock.arm` | `{"override_ack"}` (optional) | `configured`, `blocked` | state |
| `shock.start` | `{}` | `configured`, `armed` | state |
| `shock.proceed` | `{}` | `review` | state |
| `shock.fire` | `{}` | `running`, in `manual` | state |
| `shock.level` | `{"delta_db"}` (1 or -1) or `{"rung"}` (from 0) | `running` or `review`, in `manual` | `{"level_db"}` |
| `shock.train` | `{"count","spacing_s"}` (both optional: the configured train) | `running`, in `manual` | state |
| `shock.burst` | `{}` | `running`, in `manual`, with `gunfire` configured | state |
| `shock.stop` | `{}` | any | state |
| `shock.abort` | `{}` | any | state |
| `shock.reset` | `{}` | any but live | state |
| `shock.clear` | `{}` | any but live | state |
| `shock.state` | `{}` | any | state |
| `shock.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `shock.subscribe` | see "Classical Shock Display" | any | as there |
| `shock.pulses` | `{"since"}` (optional, default 0) | any | `{"pulses":[row...],"next"}` |
| `shock.pulse` | `{"index","points"}` (`points` optional) | any | `{"pulse":row,"views"}` |

"state" in the Result column is the `shock.state` object below. A run is live while it is armed,
running, holding for review or stopping. `shock.configure` replaces a run that is not live;
`shock.reset` configures the present run's configuration again on the present table and station;
`shock.clear` forgets it (`unconfigured`). A result that carries views travels as a binary frame.

### Classical Shock Configuration

```
{"title":"Half-Sine 20 g 11 ms",
 "pulse":{"shape":"half_sine","amplitude_g":20,"duration_ms":11,"polarity":"positive",
          "rise_frac":null,"fall_frac":null,
          "compensation":{"shape":"rectangular","pre_frac":4,"post_frac":4,"symmetric":true}},
 "standard":{"name":"mil_std_810_516","tolerance_abort":"full_level","overshoot_abort":null},
 "acquisition":{"fs_hz":51200,
                "srs":{"f_lo_hz":20,"f_hi_hz":2000,"bands_per_octave":3,"band_tol_db":3},
                "pvss":false},
 "schedule":{"mode":"auto","levels_db":[-12,-9,-6,-3,0],"pulses":[1,1,1,1,3],"start_db":-12,
             "advance":"on_pass","max_pulses_per_level":5},
 "spacing":{"cooling_s":1,"settling_s":0.5,"compute_s":0.25},
 "identification":{"drive_vrms":0.05,"duration_s":4,"nfft":2048,"band_hz":[5,5000],
                   "min_mean_coherence":0.8,"review":true},
 "control":{"law":{"id":"fd_correct","params":{"beta":0.5}},
            "band_hz":[5,5000],"coherence_floor":0.9,"tikhonov_floor":0.01,"ref_floor_db":50,
            "min_band_energy":0.9,"law_budget":0.25,"law_misses":3,"max_pulses":100,
            "max_peak_rise_db":10,"max_bin_rise_db":10,"first_drive_ratio":2,
            "divergence_pulses":3},
 "limits":{"drive_vpk_ceiling":0.5,"drive_vpk_abort":0.8,"peak_accel_g":null,
           "monitors":[{"channel":2,"unit":"A","peak_ceiling":40,"kind":"between_pulse"}]},
 "feasibility":{"shaker":{...},
                "moving_mass_lb":{"armature":45,"slip_table":0,"drive_adapter":3,"fixturing":10,
                                  "test_article":12},
                "residual_velocity_ips":0.1,"residual_disp_in":0.001},
 "train":{"count":5,"spacing_s":6},
 "gunfire":{"rounds_per_min":30,"rounds":3},
 "operator":"Operator","article":"Bracket","notes":"",
 "specification":{"document":"Example procedure","clause":"4.2"},"record":{"streams":true},
 "metadata":[{"key":"run_number","label":"Run Number","value":"7"}]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `pulse.shape` | (required) | `half_sine`, `haversine`, `terminal_peak_sawtooth`, `initial_peak_sawtooth`, `trapezoid` (with `rise_frac` and `fall_frac`, each a fraction of the duration, together at most 1; for no other shape) or `rectangle` (5% edges) |
| `pulse.amplitude_g`, `pulse.duration_ms` | (required) | The main pulse's peak, g (above 0, at most 10000), and its duration, ms (above 0, at most 1000) |
| `pulse.polarity` | `positive` | `negative` negates the reference; the pulse is judged on the sign-corrected record |
| `pulse.compensation` | `rectangular`, 4, 4, symmetric | The pre-pulse and post-pulse that bring the composite to rest with zero net velocity and displacement: `shape` `half_sine`, `rectangular` or `rounded_rectangular`, `pre_frac` and `post_frac` each a multiple of the duration (above 0, at most 20; `post_frac` ignored when `symmetric`). A composite that leaves the standard's quiet window is a warning (`quiet_window_exceeded`), not an error |
| `standard.name` | `mil_std_810_516` | The tolerance the pulses are judged by: `mil_std_810_516` (amplitude ±15% of peak, velocity change ±10%) or `iec_60068_2_27` (±20% and ±15%), each with a quiet window 0.4 times the duration either side of the pulse |
| `standard.tolerance_abort` | `full_level` | `full_level`: a failed pulse below full level alarms, a failed pulse at full level aborts. `every_level`: every failed pulse aborts. An undershoot never aborts; the iteration limit ends a run that does not converge |
| `standard.overshoot_abort` | null: twice the amplitude tolerance | A measured peak over `(1 + overshoot_abort)` times the commanded peak aborts at any level (0.30 under MIL-STD-810, 0.40 under IEC 60068-2-27); above 0, at most 2 |
| `acquisition.fs_hz` | 51200 | The rate requested of the station, 1000 to 204800 Hz; the run reports the rate it ran at |
| `acquisition.srs` | 20, 2000, 3, 3 | Each pulse's SRS grid (`f_lo_hz` to `f_hi_hz`, below half the rate, `bands_per_octave` bands an octave, Q 10) and its informational per-band tolerance, dB |
| `acquisition.pvss` | false | Also compute each pulse's pseudo-velocity SRS |
| `schedule.mode` | (required) | `auto` walks the levels; `manual` waits for `shock.fire` and `shock.level` |
| `schedule.levels_db` | (required) | The level table, dB, non-decreasing, from -60 to 12; the last is full level |
| `schedule.pulses` | (required) | Pulses per level: one count for every level, or one per level (1 to 1000 in `auto`) |
| `schedule.start_db` | the first level | The level a run starts at, between the first and last levels |
| `schedule.advance` | `count` | `count` moves to the next level after its pulses; `on_pass` moves only on a passing pulse |
| `schedule.max_pulses_per_level` | 5 | With `on_pass`, pulses at one level without a pass (in `manual`, failing pulses in a row at one level) before the run ends `not_converged`; 1 to 100 |
| `spacing` | 1, 0.5, 0.25 | The floors between pulse starts, seconds: `cooling_s`, `settling_s` and `compute_s`, each 0 to 3600. The time between pulses (`derived.spacing_s`) is the largest of them, above 0. A pulse starts when the station's clock starts its record, and the time between pulses holds between two such starts, from the pulse before a train to the train's first pulse too |
| `identification.drive_vrms` | 0.05 | Band-limited random at this many volts rms, below the drive ceiling; its peak is held under the drive ceiling |
| `identification.duration_s`, `nfft` | 4, 2048 | Its length, 0.5 to 120 s, and the block it is estimated in (a power of two, 256 to 65536, at most half the samples) |
| `identification.band_hz` | 5 to the lesser of 5000 Hz and 0.4 times the rate | The band it excites, below half the rate |
| `identification.min_mean_coherence` | 0.8 | A mean in-band coherence under it holds the run (`held`) |
| `identification.review` | true | Hold for the operator after identification (`review`) |
| `control.law` | `{"id":"fd_correct"}` | The control law, by its catalog `id` (see "Control Laws", world `shock_classical`), and values for the parameters its manifest declares (`params` by key; defaults fill the rest) |
| `control.band_hz` | as `identification.band_hz` | The correction band, below half the rate |
| `control.coherence_floor` | 0.9 | Bins identified under this coherence are not trusted (0 to 1) |
| `control.tikhonov_floor` | 0.01 | The inverse's floor, a fraction of the in-band peak gain (above 0, below 1) |
| `control.ref_floor_db` | 50 | Bins of the reference more than this far under its in-band peak are not trusted |
| `control.min_band_energy` | 0.9 | The fraction of the reference's energy the trusted bins must cover, or the run is held before any pulse |
| `control.law_budget` | 0.25 | One law call's time, a fraction of the time between pulses (`derived.call_budget_s`); above 0, at most 1 |
| `control.law_misses` | 3 | Unusable calls in a row that end the run `law_fault`; 1 to 100 |
| `control.max_pulses` | 100 | The run's pulse cap; 1 to 100000 |
| `control.max_peak_rise_db`, `max_bin_rise_db` | 10, 10 | The most the drive's peak, and any bin of it, may rise from one pulse to the next at full level, dB (above 0, at most 40). With `fd_correct`, `max_bin_rise_db` is at least its `max_db_per_pulse` plus 1 |
| `control.first_drive_ratio` | 2 | The first drive's predicted response peak is at most this times the reference's (1 to 10) |
| `control.divergence_pulses` | 3 | Pulses in a row at one level with the error rising more than 1 dB that end the run (`divergence`); 1 to 100 |
| `limits.drive_vpk_ceiling` | (required) | The drive ceiling, volts peak (above 0, at most 10). A commanded drive over it is scaled down to it and the action is logged |
| `limits.drive_vpk_abort` | (required) | The drive abort, volts peak, above the ceiling and at most 10. A commanded drive over it is refused and the run aborts (`drive_vpk`) |
| `limits.peak_accel_g` | null | A predicted response peak over it scales the pulse down before it fires |
| `limits.monitors` | none | Monitored inputs by table `index`, each with its unit, peak ceiling and `kind` (`predictive` or `between_pulse`) |
| `feasibility` | null | Random's shaker record and moving mass, and the composite's residual velocity (in/s) and displacement (in) tolerances (default 0.1 and 0.001). Checked at arm; the run's kinematic aborts are the shaker's ratings. Without it, feasibility is not evaluated and the run says so |
| `train` | one pulse | A train's pulse count (1 to 1000) and spacing, seconds (at least `derived.spacing_s` when more than one); `shock.train` without params fires it |
| `gunfire` | null | A gunfire burst (`shock.burst`): rounds per minute (its spacing, 60 / rounds per minute, at least `derived.spacing_s`) and rounds (1 to 10000) |
| `operator`, `article`, `notes`, `specification`, `record` | empty, null, `{"streams":true}` | The run's record, as Random's (see "Random Test Configuration" and "Run Folders"): the operator's words, what the test is run to, and whether the record holds the inputs and the drive as streams. None of them changes how the run drives |
| `metadata` | as shown | Random's |

A configuration that gives `capture` is refused `bad_params` at `config.capture`: every classical
shock run is recorded in its run folder (see "Run Folders"), and the module has no capture of its
own.

The control channel is the table's first enabled `control` row; the drive is its first enabled
`drive` row. A configuration whose shape is wrong (a missing part, a wrong type, a name that is not
one of a field's values) is refused `bad_params` naming the field. Its content errors are refused
`config_invalid` with the items, located by `field`; `dry_run: true` checks without replacing
anything and needs no control.

**Result.** `state` is the `shock.state` object (`configured`). `derived`:

```
{"grid":{"fs_hz":51200,"record_len":8192,"actual":false},
 "reference":{"peak_g":20,"velocity_change_ips":53.9588,"displacement_pk_pk_in":0.687732,
              "main_start":2534,"main_len":563},
 "quiet_window":{"worst_frac":0.0795,"limit_frac":0.15,"inside":true},
 "spacing_s":1,"call_budget_s":0.25,
 "feasibility":{"verdict":"pass","message":"The pulse is within the shaker's ratings."}}
```

- `grid`: the record every pulse is played and measured as: the composite with the quiet surround
  on both sides (half the duration, at least 5 ms), padded to a power of two. `actual` is true once
  the run has armed and the rate is the one the station runs at.
- `reference`: the composite at full level: its peak, the main pulse's velocity change (the
  tolerance check's nominal, signed by the polarity), the composite's peak-to-peak displacement,
  and where the main pulse sits on the record.
- `quiet_window`: the composite's worst value in the standard's quiet window as a fraction of the
  peak, against the standard's amplitude tolerance.
- `spacing_s`: the time between pulse starts; `call_budget_s`: one law call's time.
- `feasibility`: the composite at full level against the shaker's ratings, before any station.

`views` are the **Pulse** (the reference with its tolerance band), **Velocity** and
**Displacement** views of the reference at full level, as `shock.preview` gives them.
`shock.preview` computes the same `derived.reference`, `derived.quiet_window` and views for a
pulse alone (`pulse` and `standard` as in a configuration, `fs_hz` default 51200), with no station
and no configuration, so a screen draws a pulse without computing it.

Item reasons (Classical Shock): errors `out_of_range`, `levels_empty`, `levels_not_ascending`,
`pulses_length_mismatch`, `band_above_grid`, `record_too_long` (one pulse's record is as long as
the controller timeout), `spacing_below_floor` (a train or gunfire spacing below the time between
pulses), `limit_unknown_channel` (a monitor on a channel that is not an enabled input),
`no_control_channel`, `no_drive_channel`, `extra_drive_channels` (more than one enabled drive row:
the station would open every one as an output and split the one pulse across them), `law_unknown`,
`law_wrong_world`, `law_param_unknown`, `law_param_invalid`; warnings `quiet_window_exceeded`.

### Running a Classical Shock Test

- **`shock.arm`** checks feasibility (when given), opens the station, adopts its rate, derives the
  grid at that rate, passes the arm gate and claims the run's folder (see "Run Folders"; refused
  `record_unwritable` when it cannot be written). Nothing is driven. A pulse over the shaker's ratings
  is refused `feasibility_exceeded` and the state becomes `blocked`; `shock.arm` with a non-empty
  `override_ack` arms past it (`override_not_needed` when nothing blocks). An imported law without
  a passed check for its exact file is refused `law_not_checked` on a hardware station. The
  station's own refusals are Random's (`arm_refused`, `hardware_busy`, ...).
- **`shock.start`** from `configured` arms first. It runs identification (`phase` `sysid`), then
  holds for review (`review`) or, without a review, goes on as `shock.proceed` does. It answers at
  once.
- **`shock.proceed`** seeds the law. If the predicted full-level drive is over the drive abort,
  the run ends `aborted` (`drive_vpk`, `predicted: true`) before any pulse. In `auto` it then
  fires the schedule; in `manual` it waits for `shock.fire`.
- **`shock.fire`** fires one pulse at the present level. Refused `wrong_mode` in `auto`, and
  `not_yet` (with `seconds`) before the time between pulses has passed or while a pulse is in
  flight.
- **`shock.level`** steps ±1 dB (`delta_db`) or goes to a level of the table (`rung`, from 0),
  between the first and last levels, while the run waits between pulses or holds for review.
  Refused `wrong_mode` in `auto`, `not_a_rung` (a rung the table does not have, or a step past its
  first or last level), and `drive_ceiling` when the identified drive at the new level would pass
  the drive abort (with `vpk`, `abort_vpk` and `level_db`).
- **`shock.train`** fires `count` pulses `spacing_s` apart and **`shock.burst`** the configured
  gunfire burst, at the present level, each firing the drive in force with no law call inside it.
  Every pulse is judged. A train's spacing under the time between pulses is refused `bad_params`;
  `shock.burst` without `gunfire` is refused `wrong_state`; both are refused `wrong_mode` and
  `not_yet` as `shock.fire` is.
- **`shock.stop`** ends the run at the next boundary: the pulse in flight finishes and nothing
  more fires (`stopping`, then `operator_stop`). **`shock.abort`** takes the drive down with the
  station's own stop at once (`operator_abort`). The committed output queue is longer than a
  pulse, so an abort governs the next pulse, not the one in flight. Both reach a fire in flight.
- **When nobody holds control**, firing pauses at once (a train in progress ends at its next
  pulse); a claim within `timeout_s` resumes it, and otherwise the run ends `safe_stop`.

### Classical Shock Run State

```
{"run_id":"s-0003","state":"running","phase":"waiting","title":"Half-Sine 20 g 11 ms",
 "source":"SIM","station":{"kind":"sim","class":"sim","devices":[]},
 "grid":{"fs_hz":51200,"record_len":8192,"actual":true},"clock":"shared",
 "mode":"auto","levels_db":[-12,-9,-6,-3,0],"level_db":-6,"rung":2,
 "pulses":{"fired":3,"passed":3,"at_level":0,"of_level":1},"next_pulse_s":0.6,
 "elapsed_s":12.5,"alarm":false,
 "identification":{"drive_vrms":0.05,"drive_vpk":0.21,"coherence_mean":0.998,
                   "min_mean_coherence":0.8,"passed":true},
 "prediction":{"drive_vpk":0.4,"drive_vpk_ceiling":0.5,"drive_vpk_abort":0.8,
               "response_peak_g":20.1,"velocity_pk_ips":27.3,"displacement_pk_pk_in":0.69},
 "last_pulse":{row},
 "law":{"id":"fd_correct","name":"frequency-domain correction","version":"0.1.0","sha256":null,
        "misses":0},
 "feasibility":{"verdict":"pass","message":"..."},"end":null}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `blocked` | `feasibility_blocked` | Feasibility exceeded; arming needs an acknowledgement |
| `armed` | `armed` | Station open and gated; nothing driven yet |
| `running` | `sysid`, `waiting`, `firing` | Identification; waiting out the time to the next pulse (or for `shock.fire`); a pulse being brought up and playing (in `auto` this begins a moment before the pulse starts, at most 1 s: the station is brought up with the pulse loaded and starts it on the time between pulses) |
| `review` | `awaiting_review` | Identification done; nothing fires until `shock.proceed` |
| `stopping` | `stopping` | The pulse in flight finishes; nothing more fires |
| `finished` | `stopped`, `aborted`, `held` | Ended; `end` says how |

- `level_db` is the commanded level (null before the run starts); `rung` its place in
  `levels_db` (null when a manual step has left the table's levels).
- `pulses`: `fired` and `passed` over the run; `at_level` pulses fired at this level, and in
  `auto` `of_level` the pulses the schedule fires here (0 in `manual`).
- `next_pulse_s`: in `auto`, the seconds until the next pulse fires; in `manual`, until
  `shock.fire` is accepted (0 once it is); null when no pulse is due. It counts from the last
  pulse's start, the moment the station's clock started its record.
- `identification` and `prediction` are null until identification is done. `identification`
  is the drive it played (rms and peak, volts) and its mean in-band coherence against the hold.
  `prediction` is the predicted full-level drive peak against the ceiling and the abort, and the
  predicted response's peak, peak velocity and peak-to-peak displacement.
- `last_pulse` is the last pulse's row (below); `law` the run's law and its unusable calls in a
  row now; `feasibility` is null until arm.
- `end` is null until the run finishes, then `{"outcome","message","at_s","level_db","pulse",
  "trip"}`: `at_s` from the start, `pulse` the pulse it ended at (null before any).
- `record` is absent until the run arms, then Random's `{"folder","streams","problem"}`.

| `outcome` | How the run ended |
|---|---|
| `completed` | The schedule finished |
| `operator_stop` | `shock.stop` |
| `operator_abort` | `shock.abort` |
| `safe_stop` | Nobody held control for `timeout_s`, or the service was shut down; `message` says which |
| `aborted` | A protection tripped; `trip` says which |
| `not_converged` | The iteration limit: `max_pulses_per_level` pulses at one level without a pass, or the run's `max_pulses` |
| `held` | Identification held the run before any pulse: its mean in-band coherence was under `min_mean_coherence`, or the bins it trusts cover less than `min_band_energy` of the reference's energy; `message` says which |
| `law_fault` | The law gave no usable drive `law_misses` times in a row, or its seed was refused; nothing more fired. `trip` says why |
| `fault` | The run or the station failed; `message` is the controller's words |

`trip` is null unless `outcome` is `aborted`, `not_converged` or `law_fault`; it has `kind`,
`pulse`, `at_level_db` and its kind's facts. `predicted: true` marks a trip found on the
prediction before a pulse fired, so nothing was driven for it.

| `kind` | Facts |
|---|---|
| `tolerance` | `envelope_pass`, `velocity_change_pass`, `error_pct`, `worst_deviation_frac` |
| `overshoot` | `peak_ratio`, `limit_ratio` |
| `kinematic` | `axis` (`velocity` or `displacement`), `value`, `limit`, `predicted` |
| `drive_vpk` | `vpk`, `abort_vpk`, `predicted` |
| `accel` | `predicted_g`, `limit_g` (a predicted response peak the pre-fire check could not bring under the ceiling) |
| `monitor` | `channel`, `value`, `limit` |
| `divergence` | `pulses`, `error_pct` |
| `not_converged` | `limit` (`per_level` or `run`), `pulses` |
| `law_fault` | `reason`, `consecutive` |

A `law_fault` trip's `reason` is `error` (the law reported one, or could not be built),
`panicked` (it panicked or, for a law file, trapped), `over_budget`, `over_memory` (a law file),
`wrong_length`, `non_finite`, `undeclared_value`, `too_many_values`, `peak_rise`, `bin_rise` (the
per-pulse caps), or `first_drive` (the seed's predicted response peak over its bound; nothing
fired).

### Classical Shock Pulses

Every pulse fired has a row. `shock.pulses` returns the rows from `since` (the row `index`, from
0) and `next`, the index after the last. `shock.pulse` returns one row and its views: **Pulse**
(the reference at the level fired with its tolerance band, and the measured pulse), **Drive** (the
drive as played), **Velocity**, **Displacement** and **SRS**, each time history at most `points`
points (default 1024, 64 to 8192; a longer one is sent as an envelope, `y` the largest and
`y_min` the smallest value in each point's interval). The views of the most recent 32 pulses are
held; an older pulse's are refused `not_found`, its row is still in `shock.pulses`.

```
{"index":3,"origin":"ladder","scheduled_db":-3,"commanded_db":-3,"limit_scale":1,
 "drive_vpk":0.283,"peak_g":14.1,"peak_ratio":0.998,"error_pct":1.9,
 "velocity_change_ips":37.9,"velocity_change_ratio":1.004,
 "verdicts":{"envelope":true,"velocity":true,"substitution":true},"passed":true,
 "srs":{"bands":21,"passed_bands":21},"at_s":9.75,
 "law":{"used":true,"miss":null,"capped_bins":0,"peak_capped":false,"call_ms":1.4}}
```

- `origin` is `ladder`, `train` or `gunfire`; `scheduled_db` the level the schedule asked for and
  `commanded_db` the level fired (lower when a limit scaled it); `limit_scale` the drive-limit
  scale applied (1 when none); `drive_vpk` the drive's peak as played, volts.
- `peak_g`, `peak_ratio` (measured over commanded peak), `error_pct` (RMS error over the judged
  window, percent of the reference), `velocity_change_ips` and `velocity_change_ratio` (over the
  nominal), the three `verdicts` and `passed` are the core's judgement; null when the pulse was
  not measured.
- `srs` counts the SRS bands within the informational band; `at_s` is the pulse's start, the
  moment the station's clock started its record, in seconds from when the run armed.
- `law` is the call that produced the drive this pulse played (`used` false and `miss` the
  reason when the drive in force was refired after a miss); null for a pulse fired in force by a
  train or burst.

### Classical Shock Display

Event topics:

- `shock.state` (text): the state object on every change.
- `shock.log` (text): new log entries, as Random's.
- `shock.live` (binary, to every connection, 5 times a second from arm until the run has
  finished, then once more): `{"run_id","source","state","phase","mode","level_db","rung",
  "pulses","next_pulse_s","fire"}`, `fire` being `{"done","of"}` (frames of the pulse in flight
  played, and of the record) or null between pulses.
- `shock.pulse` (binary, to every connection, once per pulse): `{"run_id","pulse":row,"views"}`
  with the **Pulse**, **Drive** and **SRS** views at 1024 points.
- `shock.data` (binary, to one subscribed connection): `shock.live`'s fields and `plots`, once
  per pulse, once when identification ends, and at once on subscribe (so a screen that joins late
  has the latest pulse), at most `rate_hz` frames a second.

`shock.subscribe` takes Random's form: `{"rate_hz","plots":[plot...]}` (`rate_hz` 0.5 to 10,
default 5; at most 64 plots) or `{"default":true}` (no `shock.data`). A plot is
`{"id","quantity","points"}`, `points` for the time histories only (default 1024, 64 to 8192).
The result is `{"default","rate_hz","plots","quantities"}`, every default filled in. In
`shock.data` each plot comes back as `{"id","quantity","views"}`.

| `quantity` | Views | What they show |
|---|---|---|
| `pulse` | **Pulse**, **Drive** (`spectrum`, time) | The reference at the level fired, its tolerance band and the measured pulse, g; the drive as played, V |
| `velocity`, `displacement` | one `spectrum` (time) | The measured pulse's and the reference's |
| `srs` | one `spectrum` | The measured SRS and the reference's at the level fired, with the informational band |
| `spectrum` | one `spectrum` | `abs(R)` and `abs(Y)` on the pulse grid, in the correction band |
| `identification` | **Identification Gain**, **Identification Phase**, **Coherence** (`spectrum`s) | `abs(H)` (control unit per volt) and its phase, degrees; the coherence, the trusted bins and the floor |
| `law` | one `reading` | The law's reported values on its last call |
| `readings` | one `reading` | The run's numbers |
| `pulses` | one `table` | The pulse rows |

A plot of the latest pulse is empty (`views` empty) until a pulse has fired, and
`identification` until identification has ended.

## SRS Shock

An SRS shock test synthesizes each pulse to match a target shock response spectrum (SRS) and
closes the loop between pulses. The service synthesizes the target offline into a table of
components (odd-half-cycle wavelets or damped sines, one per band by default), identifies the
system with a burst of that synthesis at a stated peak in volts, and seeds the control law from
the identified gain at each component; after each pulse is fired, measured and judged, the law
proposes a gain for each component, and the service caps the gains and checks the drive before it
plays. The drive is in volts throughout. The run is one test like any other: an SRS shock run, a
classical shock run, a Random run, a MIMO run, a Sine run, a TWR run, an Explorer run, a Recorder
run and a `sim.start` run exclude each other, each refused `run_active` while another is live. **Any connected screen may stop or abort**
(`srs.stop`, `srs.abort`); `srs.configure` (other than a dry run), `arm`, `start`, `proceed`,
`fire`, `level`, `reset` and `clear` need control; everything that only reads or only computes is
open to every connection. While the service is shutting down, `srs.arm`, `start`, `proceed`,
`fire` and `level` are refused `shutting_down`.

| Method | Params | From | Result |
|---|---|---|---|
| `srs.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived","views"}` |
| `srs.config` | `{}` | any | `{"run_id","config","derived","views"}` |
| `srs.preview` | `{"target","bands","synthesis","envelope","fs_hz"}` (`synthesis`, `envelope` and `fs_hz` optional) | any | `{"derived","views"}` |
| `srs.target.import` | `{"path"}` | any | `{"target","errors","warnings"}` |
| `srs.table.import` | `{"path","engine"}` | any | `{"components","errors","warnings"}` |
| `srs.arm` | `{"override_ack"}` (optional) | `configured`, `blocked` | state |
| `srs.start` | `{}` | `configured`, `armed` | state |
| `srs.proceed` | `{}` | `review` | state |
| `srs.fire` | `{}` | `running`, in `manual` | state |
| `srs.level` | `{"delta_db"}` (1 or -1) or `{"rung"}` (from 0) | `running` or `review`, in `manual` | `{"level_db"}` |
| `srs.stop` | `{}` | any | state |
| `srs.abort` | `{}` | any | state |
| `srs.reset` | `{}` | any but live | state |
| `srs.clear` | `{}` | any but live | state |
| `srs.state` | `{}` | any | state |
| `srs.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `srs.subscribe` | see "SRS Shock Display" | any | as there |
| `srs.pulses` | `{"since"}` (optional, default 0) | any | `{"pulses":[row...],"next"}` |
| `srs.pulse` | `{"index","points"}` (`points` optional) | any | `{"pulse":row,"views"}` |

"state" in the Result column is the `srs.state` object below. A run is live while it is armed,
running, holding for review or stopping. `srs.configure` replaces a run that is not live;
`srs.reset` configures the present run's configuration again on the present table and station;
`srs.clear` forgets it (`unconfigured`). A result that carries views travels as a binary frame.
SRS shock has no trains and no gunfire bursts: every pulse is a synthesized pulse of the schedule.

### SRS Shock Configuration

```
{"title":"SRS 100 g, 100 to 2000 Hz",
 "target":{"kind":"breakpoints","points":[[100,40],[1000,100],[2500,100]]},
 "bands":{"f_lo_hz":100,"f_hi_hz":2000,"bands_per_octave":3,"q":10},
 "synthesis":{"engine":"wavsyn","components":null,"max_iters":50,"tol_db":1.5,
              "max_gain_db_per_iter":3},
 "envelope":{"alarm_db":[3,3],"abort_db":[9,6],"per_band":null},
 "acquisition":{"fs_hz":51200,"ring_out_s":0.05,"pvss":true},
 "schedule":{"mode":"auto","levels_db":[-12,-6,0],"pulses":[2,2,3],"start_db":-12,
             "advance":"on_pass","max_pulses_per_level":5},
 "spacing":{"cooling_s":1,"settling_s":0.5,"compute_s":0.25},
 "identification":{"excitation":"shaped","level_db":-20,"max_vpk":null,"nfft":1024,
                   "h_floor_frac":0.05,"min_mean_coherence":0.8,"review":true},
 "control":{"law":{"id":"srs_ratio","params":{"max_db_per_pulse":3}},
            "law_budget":0.25,"law_misses":3,"max_pulses":100,"max_gain_rise_db":6},
 "limits":{"drive_vpk_ceiling":0.5,"drive_vpk_abort":0.8,"peak_accel_g":null,"abort_factor":2,
           "kinematic":{"accel_g":null,"velocity_ips":null,"disp_pp_in":null},
           "monitors":[{"channel":2,"unit":"A","peak_ceiling":40,"kind":"between_pulse"}]},
 "limit_channels":{"headroom_db":1,
                   "channels":[{"channel":3,"bound":{"kind":"flat","accel_g":50},"frf":null}]},
 "feasibility":{"shaker":{...},
                "moving_mass_lb":{"armature":45,"slip_table":0,"drive_adapter":3,"fixturing":10,
                                  "test_article":12},
                "residual_velocity_ips":0.1,"residual_disp_in":0.001},
 "operator":"Operator","article":"Bracket","notes":"",
 "specification":{"document":"Example procedure","clause":"4.2"},"record":{"streams":true},
 "metadata":[{"key":"run_number","label":"Run Number","value":"7"}]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `target` | (required) | The target SRS, g: `{"kind":"flat","accel_g"}` (every band the same, above 0, at most 1000000), or `{"kind":"breakpoints","points":[[frequency_hz, accel_g], ...]}`, at least two points, frequencies strictly ascending, both above 0, joined by straight lines on log-log axes and covering the band grid from `f_lo_hz` to its top centre (`srs.target.import` reads such a table from a file) |
| `bands` | (required: `f_lo_hz`, `f_hi_hz`) | The band grid every spectrum is on: centres from `f_lo_hz` up to the first centre at or above `f_hi_hz`, that top centre below half the rate; `bands_per_octave` centres an octave (default 3; a whole number from 1 to 24), and the quality factor `q` every SRS is computed at (default 10, above 0.5, at most 100) |
| `synthesis.engine` | `wavsyn` | `wavsyn`: odd-half-cycle wavelets, at rest at both ends by construction. `seds`: damped sines with the compensating terms that bring the composite to rest |
| `synthesis.components` | null: one per band | The component table to start from: `{"engine","rows":[...]}` (`srs.table.import` reads one from a file), 1 to 512 rows. A `wavsyn` row is `{"freq_hz","amplitude_g","half_cycles","delay_s"}` (`half_cycles` odd, at least 3; `delay_s` default 0); a `seds` row is `{"freq_hz","amplitude_g","zeta","delay_s","polarity"}` (`zeta` above 0, `polarity` `positive` (the default) or `negative`). Its `engine` is `synthesis.engine` |
| `synthesis.max_iters`, `tol_db`, `max_gain_db_per_iter` | 50, 1.5, 3 | The offline synthesis that fits the table to the target before any station: at most `max_iters` passes (1 to 1000), done when every band is within `tol_db` (above 0, at most 20), each pass moving a component at most `max_gain_db_per_iter` (above 0, at most 20). It does not set the run's law |
| `envelope` | ±3 dB alarm; abort 9 dB under, 6 dB over | The tolerance on every band, judged against the target at the level fired: `alarm_db` and `abort_db` as `[under, over]`, the dB a band may sit under and over the target (each above 0, at most 60, the abort at least as wide as the alarm on each side), or `per_band`, one `{"alarm_db","abort_db"}` per band in place of both. A band outside the abort band aborts the run (`envelope`); outside the alarm band it alarms |
| `acquisition.fs_hz` | 51200 | The rate requested of the station, 1000 to 204800 Hz; the run reports the rate it ran at |
| `acquisition.ring_out_s` | 0.05 | Seconds measured after each pulse's drive ends, 0 to 10 |
| `acquisition.pvss` | true | Also compute each pulse's pseudo-velocity SRS |
| `schedule` | as classical shock's | `mode`, `levels_db`, `pulses`, `start_db`, `advance` and `max_pulses_per_level` as "Classical Shock Configuration" has them. With `advance` `on_pass` a pulse passes when every band is inside the alarm band, but for a band a limit channel holds down reading low (see "SRS Shock Pulses") |
| `spacing` | 1, 0.5, 0.25 | The floors between pulse starts, as classical shock's (`derived.spacing_s` is the largest) |
| `identification.excitation` | `shaped` | `shaped`: the synthesized pulse, which puts the identification's energy where the test drives. `standard_pulse`: a short broadband pulse |
| `identification.level_db` | -20 | The burst's level under the full-level synthesis, dB (below 0, at least -60) |
| `identification.max_vpk` | null: a tenth of the drive ceiling | The burst's peak in volts is at most this (above 0, at most the drive ceiling); a burst over it is scaled down to it |
| `identification.nfft` | 1024 | The block the gain is estimated in (a power of two, 64 to 65536) |
| `identification.h_floor_frac` | 0.05 | The identified gain is floored at this fraction of its in-band peak before the seed divides by it (above 0, at most 1) |
| `identification.min_mean_coherence` | 0.8 | A mean in-band coherence under it holds the run (`held`), 0 to below 1 |
| `identification.review` | true | Hold for the operator after identification (`review`) |
| `control.law` | `{"id":"srs_ratio"}` | The control law, by its catalog `id` (see "Control Laws", world `shock_srs`), and values for the parameters its manifest declares (`params` by key; defaults fill the rest) |
| `control.law_budget`, `law_misses`, `max_pulses` | 0.25, 3, 100 | As classical shock's |
| `control.max_gain_rise_db` | 6 | The most any component's gain may rise from one pulse to the next, dB (above 0, at most 20); a law asking for more is not used (`gain_rise`). With `srs_ratio` it is at least its `max_db_per_pulse` plus 1 |
| `limits.drive_vpk_ceiling` | (required) | The drive ceiling, volts peak (above 0, at most 10). A commanded drive over it is scaled down to it and the action is logged |
| `limits.drive_vpk_abort` | (required) | The drive abort, volts peak, above the ceiling and at most 10. A commanded drive over it is refused and the run aborts (`drive_vpk`) |
| `limits.peak_accel_g` | null | A predicted response peak over it scales the pulse down before it fires; at `abort_factor` times it the run aborts (`accel`) |
| `limits.abort_factor` | 2 | A predicted response peak or a monitored peak at or over this times its ceiling aborts instead of scaling (1 to 100) |
| `limits.kinematic` | none | Per-pulse ceilings on the measured, and before each pulse the predicted, peak acceleration (g), peak velocity (in/s) and peak-to-peak displacement (in), each above 0; null is no ceiling |
| `limits.monitors` | none | Monitored inputs by table `index`, as classical shock's |
| `limit_channels` | null | Inputs whose response must stay under a bound: `headroom_db` (0 to 40) how far under its bound a prediction must stay, and `channels`, each a table `index` (any row but the control row; without an `frf`, an enabled input), a `bound` (`{"kind":"flat","accel_g"}` or `{"kind":"per_band","bounds_g":[...]}`, one per band, null for no bound at a band) and an optional `frf` (`{"freqs_hz":[...],"mag":[...]}`, the drive-to-channel gain in g per volt, frequencies ascending from 0; null: identification measures it). At proceed the service notches the drive where a prediction would pass a bound, and before each pulse caps every component by what the prediction leaves. An empty `channels` list is no limit channels |
| `feasibility` | null | Random's shaker record and moving mass, and the synthesis's residual velocity (in/s) and displacement (in) tolerances (default 0.1 and 0.001). Checked at arm. Without it, feasibility is not evaluated and the run says so |
| `operator`, `article`, `notes`, `specification`, `record` | empty, null, `{"streams":true}` | The run's record, as Random's (see "Random Test Configuration" and "Run Folders"): the operator's words, what the test is run to, and whether the record holds the inputs and the drive as streams. None of them changes how the run drives |
| `metadata` | the default fields | Random's |

A configuration that gives `capture` is refused `bad_params` at `config.capture`: every SRS shock
run is recorded in its run folder (see "Run Folders"), and the module has no capture of its own.

The control channel is the table's first enabled `control` row; the drive is its first enabled
`drive` row. A monitor and a limit channel are named to the controller by their row's
`description`, or `ch<index>` without one. A configuration whose shape is wrong is refused
`bad_params` naming the field; its content errors are refused `config_invalid` with the items,
located by `field`; `dry_run: true` checks without replacing anything and needs no control.

**Result.** `state` is the `srs.state` object (`configured`). `derived`:

```
{"grid":{"fs_hz":51200,"record_len":10569,"actual":false},
 "bands_hz":[100,125.9921,158.7401,...,2015.8737],"target_g":[40,43.8521,48.0752,...,100],
 "synthesis":{"engine":"wavsyn","components":14,"iters":9,"converged":true,"worst_err_db":-0.893,
              "duration_s":0.15643},
 "reference":{"peak_g":22.2066,"velocity_pk_ips":6.1726,"displacement_pk_pk_in":0.020336,
              "residual_velocity_ips":0,"residual_disp_in":0},
 "spacing_s":1,"call_budget_s":0.25,
 "feasibility":{"verdict":"pass","message":"The pulse is within the shaker's ratings."}}
```

- `grid`: every pulse's record, the synthesized pulse and its ring-out, at the requested rate;
  `actual` is true once the run has armed and the rate is the one the station runs at.
- `bands_hz` and `target_g`: the band grid and the full-level target on it.
- `synthesis`: the offline fit of the table to the target: its components, passes, whether every
  band reached `tol_db`, the worst band's error, dB (signed), and the pulse's length, seconds.
- `reference`: the synthesized pulse at full level, g: its peak, peak velocity, peak-to-peak
  displacement and what it leaves at its end.
- `spacing_s`, `call_budget_s` and `feasibility` as classical shock's.

`views` are **Target** (the target with its alarm and abort bands), **Synthesis** (the
synthesized pulse's SRS against the target), **Pulse**, **Velocity** and **Displacement** (the
synthesized pulse at full level from its first sample, series `Synthesized`), as `srs.preview`
gives them. `srs.preview` computes the same `derived.bands_hz`, `target_g`, `synthesis` and
`reference` and the views for a target alone (`target`, `bands`, `synthesis` and `envelope` as
in a configuration, `fs_hz` default 51200), with no station and no configuration, so a screen
draws a synthesis without computing it. A preview is a computation, never a measurement; a
preview whose parts are not sound is refused `bad_params` naming the first.

`srs.target.import` reads a breakpoint table (two columns, frequency in Hz and SRS in g; tab,
comma, semicolon or space separated; a header row and `#` comments allowed) and
`srs.table.import` a component table for `engine` (`wavsyn` or `seds`, the rows' fields as
above, by header name or in that order), each by full path on the computer that runs the test,
at most 4 MiB. Neither needs control or changes the run; each returns what a configuration
takes (`target`, `components`), or null with error items (`target_file`, `table_file`) at the
data `row` they name (from 0, its line in `message`), or at `field` `path` for a file that could
not be read. A relative path is `bad_params`.

Item reasons (SRS Shock): errors `out_of_range`, `levels_empty`, `levels_not_ascending`,
`pulses_length_mismatch`, `band_above_grid`, `bands_empty`, `target_not_covering` (the breakpoints
do not cover the band grid), `target_not_ascending`, `alarm_wider_than_abort`,
`per_band_length_mismatch` (a per-band envelope or bound list that is not one per band),
`components_engine_mismatch`, `component_invalid` (at `synthesis.components.rows[i]`),
`record_too_long`, `limit_unknown_channel`, `limit_no_frf` (a limit channel with no `frf` and no
input to measure one from), `no_control_channel`, `no_drive_channel`, `extra_drive_channels` (more
than one enabled drive row, as in Classical Shock), `law_unknown`,
`law_wrong_world`, `law_param_unknown`, `law_param_invalid`; warnings `synthesis_not_converged`
(the offline fit did not reach `tol_db`; the run's law refines from there).

### Running an SRS Shock Test

- **`srs.arm`** checks feasibility (when given) on the synthesized pulse, opens the station, adopts
  its rate, derives the grid at that rate, passes the arm gate and claims the run's folder (see
  "Run Folders"; refused `record_unwritable` when it cannot be written). Nothing is driven. Refusals as
  `shock.arm`'s: `feasibility_exceeded` (the state becomes `blocked`; a non-empty `override_ack`
  arms past it), `override_not_needed`, `law_not_checked` on a hardware station, and the
  station's own.
- **`srs.start`** from `configured` arms first. It runs identification (`phase` `sysid`): the
  burst at `identification.level_db`, its peak held at `identification.max_vpk`, measuring the
  gain to the control channel and to every limit channel without an `frf`. Then it holds for
  review (`review`) or, without a review, goes on as `srs.proceed` does. It answers at once.
- **`srs.proceed`** seeds the law, then plans the limit notch at full level when there are limit
  channels. If the predicted full-level drive is over the drive abort, the run ends `aborted`
  (`drive_vpk`, `predicted: true`) before any pulse. In `auto` it then fires the schedule, a pulse
  every `derived.spacing_s` at the soonest; in `manual` it waits for `srs.fire`.
- **`srs.fire`** and **`srs.level`** are `shock.fire` and `shock.level` for SRS, with the same
  refusals (`wrong_mode`, `not_yet` with `seconds`, `not_a_rung`, `drive_ceiling`).
- **Before each pulse** the service synthesizes the drive from the table, scales it to the level
  fired, predicts the response through the identified gain and checks the drive in volts against
  the ceiling and abort, the predicted peak against `peak_accel_g` and the predicted kinematics
  against `limits.kinematic` and the shaker's ratings. A predicted trip fires nothing.
- **After each pulse** it judges the measured SRS against the envelope, the measured kinematics
  and the monitors, then calls the law and caps its gains: by `max_gain_rise_db`, and by the limit
  channels' prediction (a gain may rise only as far as the tightest channel near the component
  can take, and a binding channel takes it down; a falling gain always wins).
- **`srs.stop`**, **`srs.abort`** and **when nobody holds control** are as classical shock's.

### SRS Shock Run State

```
{"run_id":"srs-0004","state":"running","phase":"waiting","title":"SRS 100 g, 100 to 2000 Hz",
 "source":"SIM","station":{"kind":"sim","class":"sim","devices":[]},
 "grid":{"fs_hz":51200,"record_len":14336,"actual":true},"clock":"shared",
 "mode":"auto","levels_db":[-12,-6,0],"level_db":-6,"rung":1,
 "pulses":{"fired":3,"passed":2,"at_level":1,"of_level":2},"next_pulse_s":0.6,
 "elapsed_s":9.5,"alarm":false,
 "identification":{"excitation":"shaped","drive_vpk":0.05,"max_vpk":0.05,
                   "coherence_mean":0.97,"min_mean_coherence":0.8,"passed":true},
 "prediction":{"drive_vpk":0.41,"drive_vpk_ceiling":0.5,"drive_vpk_abort":0.8,
               "response_peak_g":72.1,"velocity_pk_ips":14.1,"displacement_pk_pk_in":0.031},
 "notch":{"notched_bands":2,"worst_before_db":3.1,"worst_after_db":-0.6,"converged":true},
 "last_pulse":{row},
 "law":{"id":"srs_ratio","name":"SRS ratio","version":"0.1.0","sha256":null,"misses":0},
 "feasibility":{"verdict":"pass","message":"..."},"end":null}
```

`state`, `phase`, `level_db`, `rung`, `pulses`, `next_pulse_s`, `prediction`, `last_pulse`,
`law`, `feasibility` and `end` are as "Classical Shock Run State" has them; `prediction` is
the table in force's, renewed after every pulse as the law moves the table. `pulses.passed` counts
the pulses that passed (`passed` in their rows). `identification` is null until identification is
done, then the excitation, the burst's peak and its cap (volts), and its mean in-band coherence
against the hold. `notch` is null without limit channels, then the arm-time notch: the bands it
notched, the worst predicted exceedance before and after it (dB over the allowed level) and
whether its fit converged. `record` is absent until the run arms, then Random's
`{"folder","streams","problem"}`.

| `outcome` | How the run ended |
|---|---|
| `completed` | The schedule finished |
| `operator_stop`, `operator_abort`, `safe_stop` | As classical shock's |
| `aborted` | A protection tripped; `trip` says which |
| `not_converged` | The iteration limit: `max_pulses_per_level` pulses at one level without a pass, or the run's `max_pulses` |
| `held` | Identification held the run before any pulse: its mean in-band coherence was under `min_mean_coherence` |
| `law_fault` | The law gave no usable gains `law_misses` times in a row, or its seed was not usable; nothing more fired. `trip` says why |
| `fault` | The run or the station failed; `message` is the controller's words |

| `kind` | Facts |
|---|---|
| `envelope` | `abort_bands`, `worst_band_hz`, `worst_err_db` |
| `kinematic` | `axis` (`accel`, `velocity` or `displacement`), `value`, `limit`, `predicted` |
| `drive_vpk` | `vpk`, `abort_vpk`, `predicted` |
| `accel` | `predicted_g`, `limit_g` (a predicted response peak at or over `abort_factor` times `peak_accel_g`) |
| `monitor` | `channel`, `value`, `limit` |
| `not_converged` | `limit` (`per_level` or `run`), `pulses` |
| `law_fault` | `reason`, `consecutive` |

A `law_fault` trip's `reason` is `error`, `panicked`, `over_budget`, `over_memory`,
`wrong_length` (not one gain per component), `non_finite`, `not_positive` (a gain at or under
zero), `undeclared_value`, `too_many_values` or `gain_rise` (a gain rising more than
`max_gain_rise_db`).

### SRS Shock Pulses

Every pulse fired has a row; `srs.pulses` and `srs.pulse` are `shock.pulses` and `shock.pulse` for
SRS. `srs.pulse`'s views are **SRS** (the measured SRS and the target at the level fired with its
alarm and abort bands), **Pulse** (the measured pulse), **Drive** (the drive as played),
**Velocity** and **Displacement**, each time history from the record's first sample and at most
`points` points as there; the views of the most recent 32 pulses are held.

```
{"index":2,"origin":"ladder","scheduled_db":-6,"commanded_db":-6,"limit_scale":1,
 "drive_vpk":0.205,"peak_g":36.3,"velocity_pk_ips":7.1,"displacement_pk_pk_in":0.016,
 "worst_err_db":-1.2,"worst_band_hz":630,"alarm_bands":0,"abort_bands":0,"passed":true,
 "limited_components":2,"worst_limit_db":-0.7,"at_s":7.5,
 "law":{"used":true,"miss":null,"call_ms":0.2}}
```

- `origin` is always `ladder`; `scheduled_db`, `commanded_db`, `limit_scale`, `drive_vpk` and
  `at_s` as classical shock's.
- `peak_g`, `velocity_pk_ips` and `displacement_pk_pk_in` are the measured pulse's kinematics.
  `worst_err_db` is the band furthest from the target at the level fired and `worst_band_hz` its
  centre; `alarm_bands` and `abort_bands` count the bands outside each band of the envelope;
  `passed` is true when no band is outside the alarm band. A band a limit channel holds down (a band
  the notch cut, or one within half an octave of it, where the cap holds the components that reach
  it) that reads low does not fail the pulse; it still alarms. A band outside the abort band, or
  reading high, always fails it. They are null when the pulse was not measured.
- `limited_components` counts the components the limit channels' prediction capped in the gains
  this pulse played, and `worst_limit_db` is the worst predicted limit-channel exceedance for its
  drive, dB over the allowed level (both null without limit channels).
- `law` is the call that produced the gains this pulse played (`used` false and `miss` the
  reason when the gains in force were kept after a miss).

### SRS Shock Display

Event topics: `srs.state`, `srs.log`, `srs.live`, `srs.pulse` and `srs.data`, as classical shock's;
`srs.pulse` carries the **SRS**, **Pulse** and **Drive** views at 1024 points. `srs.subscribe`
takes classical shock's form; `points` applies to `pulse`, `velocity` and `displacement`.

| `quantity` | Views | What they show |
|---|---|---|
| `srs` | **SRS** (`spectrum`) | The target at the level fired with its alarm and abort bands, and the measured SRS, g |
| `pulse` | **Pulse**, **Drive** (`spectrum`, time) | The measured pulse, g; the drive as played, V |
| `velocity`, `displacement` | **Velocity**, **Displacement** (`spectrum`, time) | The measured pulse's |
| `identification` | **Identification Gain**, **Coherence** (`spectrum`s) | `abs(H)` to the control channel (control unit per volt) with the floor the seed divides by at the least; the coherence with the hold |
| `components` | **Components**, **Gains** (`spectrum`s) | Each component's drive amplitude in the table in force, V, at its frequency; the gains applied to the table on the law's last call, after the service's caps (no Gains view before its first call) |
| `limit_channels` | **Limit** *name* (`spectrum`), one per limit channel | The predicted SRS for the last pulse's drive, the bound and the bound less the headroom, g (a band with no bound is left out) |
| `limit_frf` | **Gain** *name* (`spectrum`), one per limit channel | The drive-to-channel gain, g per volt, `Measured` or `Supplied` |
| `law` | one `reading` | The law's reported values on its last call |
| `readings` | one `reading` | The run's numbers |
| `pulses` | one `table` | The pulse rows |

A plot of the latest pulse is empty (`views` empty) until a pulse has fired, `identification` and
`limit_frf` until identification has ended, and `limit_channels` without limit channels.

## MIMO Shock

A MIMO Shock test fires one pulse at a time on several drives at once and judges it at several
control channels: a classical pulse at every control (`target.kind` `waveform`) or a Shock
Response Spectrum at every control (`srs`). One engine runs both. The service identifies the
plant with one shot of independent band-limited random on every drive, on the pulse's own grid;
the control law builds every drive record from it; after each pulse is fired, measured and judged
per control, the ladder schedules the next level and the law builds the next drives for that
level, which the service checks before they play. Drives are in volts throughout. The run is one
test like any other and excludes every other run (`run_active`). **Any connected screen may stop
or abort** (`mimo_shock.stop`, `mimo_shock.abort`); `mimo_shock.configure` (other than a dry
run), `arm`, `start`, `proceed`, `fire`, `reset` and `clear` need control; everything that only
reads is open to every connection. While the service shuts down, `arm`, `start`, `proceed` and
`fire` are refused `shutting_down`. Words mean what they mean in "Classical Shock" and "SRS
Shock" unless this section says otherwise.

| Method | Params | From | Result |
|---|---|---|---|
| `mimo_shock.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived","views"}` |
| `mimo_shock.config` | `{}` | any | `{"run_id","config","derived","views"}` |
| `mimo_shock.arm` | `{}` | `configured` | state |
| `mimo_shock.start` | `{}` | `configured`, `armed` | state |
| `mimo_shock.proceed` | `{"acknowledge"}` (optional) | `review` | state |
| `mimo_shock.fire` | `{}` | `running`, in `manual` | state |
| `mimo_shock.stop` | `{}` | any | state |
| `mimo_shock.abort` | `{}` | any | state |
| `mimo_shock.reset` | `{}` | any but live | state |
| `mimo_shock.clear` | `{}` | any but live | state |
| `mimo_shock.state` | `{}` | any | state |
| `mimo_shock.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `mimo_shock.subscribe` | see "MIMO Shock Display" | any | as there |
| `mimo_shock.pulses` | `{"since"}` (optional, default 0) | any | `{"pulses":[row...],"next"}` |
| `mimo_shock.pulse` | `{"index","points"}` (`points` optional) | any | `{"pulse":row,"views"}` |

"state" is the `mimo_shock.state` object below. There are no trains, gunfire bursts or manual
level changes: the ladder alone sets the level, in `manual` as in `auto`. A result that carries
views travels as a binary frame.

### MIMO Shock Configuration

```
{"title":"Four-Drive Half-Sine",
 "channels":{"controls":[{"channel":1},{"channel":2},{"channel":3},{"channel":4}],
             "drives":[{"channel":5,"ceiling_vpk":4,"abort_vpk":6},
                       {"channel":6,"ceiling_vpk":4,"abort_vpk":6},
                       {"channel":7,"ceiling_vpk":4,"abort_vpk":6},
                       {"channel":8,"ceiling_vpk":4,"abort_vpk":6}],
             "limits":[]},
 "target":{"kind":"waveform","standard":"mil_std_810_516",
           "pulses":[{"shape":"half_sine","amplitude_g":20,"duration_ms":11,"polarity":"positive",
                      "rise_frac":null,"fall_frac":null,
                      "compensation":{"shape":"rectangular","pre_frac":4,"post_frac":4,
                                      "symmetric":true}}, ...]},
 "timing":{"mode":"free","window_s":0.002,"delays_s":null},
 "verdict":"all",
 "acquisition":{"fs_hz":12800,"record_len":8192,"band_hz":[2,2500],"lead_s":0.05},
 "schedule":{"mode":"auto","levels_db":[-12,-9,-6,-3,-1.5,0],"max_pulses_per_level":3,
             "full_level_pulses":3},
 "spacing":{"cooling_s":1,"settling_s":0.5,"compute_s":0.25},
 "identification":{"drive_vrms":0.05,"frames":64,"coherence_floor":0.5,"review":true},
 "control":{"law":{"id":"inverse","params":{}},"law_budget":0.25,"seed_budget_s":60,
            "law_misses":3,"max_pulses":100,"max_growth_db":6,"max_band_growth_db":10,"divergence_pulses":3,
            "divergence_drive_ratio":1.3,"first_drive_ratio":2,"abort_db":null,
            "scaled_aborts":true,"max_velocity_ips":null,"max_displacement_in":null},
 "simulator":{"plant":null},
 "capture":{"auto_start":false,"run_name":"mimo_shock"},
 "metadata":[]}
```

An SRS target replaces `target`:

```
 "target":{"kind":"srs","bands":{"f_lo_hz":50,"f_hi_hz":2000,"bands_per_octave":3,"q":10},
           "tolerance_db":[-3,3],
           "controls":[{"target":{"kind":"breakpoints","points":[[50,3.33],[300,20],[2500,20]]},
                        "synthesis":{"engine":"wavsyn"},"polarity":"positive"}, ...]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `channels.controls` | (required) | The control channels by table `index` (each an enabled `control` row), 1 to 16. Their order is the order of every per-control list |
| `channels.drives` | (required) | The drives by table `index` (each an enabled `drive` row), 1 to 8, in the order of the table's enabled drive rows, each with its own `ceiling_vpk` (above 0, at most 10; a drive over it is scaled down to it) and `abort_vpk` (above the ceiling, at most 10; a drive over it is refused and the run aborts, `drive_abort`). On NI every enabled drive row is an output; a drive row the run does not name is refused (`drive_unknown_channel`) |
| `channels.limits` | none | Monitored inputs (enabled `monitor` or `control` rows not named as controls) by `index`, each with its `abort_peak` in the channel's unit: a measured or predicted peak over it ends the run (`limit_abort`, `predicted_abort`) |
| `target.kind` | (required) | `waveform` or `srs`; one kind for every control |
| `target.standard` | `mil_std_810_516` | For `waveform`: the classical standard every control is judged by (`mil_std_810_516` or `iec_60068_2_27`, as Classical Shock's `standard.name`) |
| `target.pulses` | (required for `waveform`) | One per control: a Classical Shock `pulse`. `polarity` `negative` plays that control's pulse upside down |
| `target.bands` | (required for `srs`) | The SRS natural frequencies every control is judged at (SRS Shock's `bands`) |
| `target.tolerance_db` | `[-3, 3]` | For `srs`: the band a control's SRS passes in, dB below and above its target (below 0, above 0) |
| `target.controls` | (required for `srs`) | One per control: its target SRS (SRS Shock's `target`), its `synthesis` (SRS Shock's) and its `polarity`. The service synthesizes a full-level waveform for each control, so a waveform law can run an SRS test |
| `timing.mode` | `free` | `free`: each control is judged after its own best-fit delay is removed, searched within `window_s` (default 0.002, above 0, at most 0.1), and the delay is reported; a spread of the controls' delays over the window is a warning. `hold`: each control's arrival must be within `window_s` of its own `delays_s` (one per control, seconds, 0 to 0.1), or it fails on `timing` |
| `verdict` | `all` | The joint verdict the ladder is fed. `all`: every control inside its own tolerance. `global` (an `srs` target only): the energy-weighted global SRS error inside the tolerance, and no control over its abort line |
| `acquisition.fs_hz` | 51200 | The rate requested of the station, 1000 to 204800 Hz; the run adopts the rate it runs at and derives the grid and the targets from it |
| `acquisition.record_len` | the shortest power of two (at least 1024) whose first three quarters hold every target from `lead_s` | Every record's length, a power of two from 64 to 16384 |
| `acquisition.band_hz` | from 5 Hz, or for a `waveform` target from 0.2 over its longest compensated pulse in seconds when that is lower (2.02 Hz for an 11 ms half-sine with shoulders 4 times its duration), to the lesser of 5000 Hz and 0.4 times the rate | The band identification excites and the law corrects, below half the rate. A `waveform` target is refused (`band_cuts_velocity_change`) when, cut to the band, a perfect delivery of a control's pulse keeps less than half the margin its own velocity change leaves to the standard's tolerance |
| `acquisition.lead_s` | 0.05 | Where every target starts in its record, seconds |
| `schedule.mode` | `auto` | `auto` fires at the time between pulses; `manual` waits for `mimo_shock.fire` |
| `schedule.levels_db` | -12, -9, -6, -3, -1.5, 0 | The levels, dB, ascending, the last 0 (full level) |
| `schedule.max_pulses_per_level` | 3 | Pulses at one level without a joint pass before the run ends `not_converged`; 1 to 100 |
| `schedule.full_level_pulses` | 3 | Passing pulses at full level that complete the run; 1 to 100. A level below full advances on its first passing pulse |
| `spacing` | 1, 0.5, 0.25 | As Classical Shock's: the time between pulse starts is the largest |
| `identification.drive_vrms` | 0.05 | The most each drive plays, volts rms, under its ceiling at its peak. The shot's level is found by a probe at a tenth of it: each control's response 20 dB under its target's rms, never over this |
| `identification.frames` | the larger of 64 and 10 times the drives | Records averaged in the one identification shot; 10 times the drives to 4096 |
| `identification.coherence_floor` | 0.5 | Lines where a control's multiple coherence is under it are not trusted (0 to 1) |
| `identification.review` | true | Hold for the operator after identification (`review`) |
| `control.law` | `inverse` for `waveform`, `shape` for `srs` | The control law by catalog `id` (world `shock_mimo`; built in: `inverse`, `shape`, `paired`) and values for the parameters its entry declares (`params` by key; defaults fill the rest). A law whose entry does not accept the target kind is refused `law_wrong_target` |
| `control.law_budget`, `law_misses`, `max_pulses` | 0.25, 3, 100 | As Classical Shock's. A law that panics ends the run at once (`law_fault`, `panicked`). Before the first pulse a second copy of the law is built, seeded and timed on one iteration over the identified plant's predicted response: a law that cannot answer a pulse inside `law_budget` ends the run then (`law_fault`, `law_too_slow`), not in the middle of the ladder |
| `control.seed_budget_s` | 60 | The time the law may take to be built and to answer its seed, seconds (above 0, at most 3600): the seed is where a law fits its model of the plant. Over it, the run ends before the first pulse (`law_fault`, `seed_over_budget`) |
| `control.max_growth_db` | 6 | The most any drive's peak may grow from one pulse to the next beyond the level step, dB (above 0, at most 40); an answer over it is not used (`peak_rise`) |
| `control.max_band_growth_db` | 10 | The same for any third-octave band of a drive (above 0, at most 40; `bin_rise`) |
| `control.divergence_pulses` | 3 | Pulses in a row at one level with the waveform error rising that end the run in the controlled stop (`divergence`); 1 to 100 |
| `control.divergence_drive_ratio` | 1.3 | A drive whose rms at one level passes this many times its rms on that level's first pulse ends the run the same way (above 1, at most 10) |
| `control.first_drive_ratio` | 2 | The seed's predicted response peak at every control is at most this times its target's at the first level (1 to 10; `first_drive`) |
| `control.abort_db` | null: twice the standard's amplitude tolerance for `waveform` (2.28 dB under MIL-STD-810), 6 dB for `srs` | Each control's abort margin, dB, above 0, at most 40. The **absolute** line is this margin over the control's target peak at **full level**: a measured peak over it aborts the run at any level (`aborted`, `control_abort_absolute`), whatever `scaled_aborts` says. The **scaled** line is the same margin over the target's peak at the level played |
| `control.scaled_aborts` | true | On: a measured peak over its scaled line ends the run in the controlled stop (`safe_stop`, `control_abort_scaled`). Off: the pulse is judged failing (the ladder does not climb on it), a warning names every control that crossed its scaled line, the run goes on, and the state carries a standing `notice`. Either way every row reports each control's margin to both lines. It cannot change while a run is live |
| `control.max_velocity_ips`, `max_displacement_in` | null | A control's peak velocity (in/s) or peak-to-peak displacement (in), predicted or measured, over it ends the run (`kinematics`) |
| `simulator.plant` | null: chosen by size | The simulator's plant by name: `shock-linear-4x4`, `shock-jointed-4x4-moderate`, `shock-jointed-4x4-severe` or `shock-jointed-6x3`. Null takes `shock-linear-4x4` for 4 controls and 4 drives and `shock-jointed-6x3` for 6 and 3. Ignored on NI |
| `capture` | as shown | `{"auto_start","run_name"}`. With `auto_start`, the run's records are held in memory and written when the run ends, however it ends, to `<run_name>_<run_id>/` under the local data root (a name already taken gets the first free `<run_name>_<run_id>-<n>`, so no run writes over another; the run id counts from 1 again each time the service starts): `setup.json` (the identified setup and the law), `law_calls.jsonl` (every law call with its inputs and its answer) and `pulses.jsonl` (every pulse's level, drives as played and input rows). Nothing is written while the run is going. With `auto_start` false nothing is written |
| `metadata` | as shown | Random's |

A configuration whose shape is wrong is refused `bad_params` naming the field; its content errors
are refused `config_invalid` with the items, located by `field` or `channel`.

**Result.** `state` (`configured`), `derived` and `warnings`. `derived`:

```
{"grid":{"fs_hz":12800,"record_len":8192,"lines":4097,"actual":false},
 "controls":[{"channel":1,"peak":20}, ...],
 "spacing_s":1,"call_budget_s":0.25}
```

`controls` holds each control's full-level target peak (EU).
`views` are each control's **Target** *n* view at full level (*n* its table index), and for an
`srs` target its **Target SRS** *n* view.

Item reasons (MIMO Shock): errors `out_of_range`, `levels_empty`, `levels_not_ascending`,
`band_above_grid`, `record_too_long` (one record is as long as the controller timeout),
`controls_out_of_range`, `drives_out_of_range`, `duplicate_channel` (a channel named twice among
the controls, drives and limits), `control_unknown_channel`, `drive_unknown_channel` (also a
table drive row the run does not name, or the drives out of the table's order),
`limit_unknown_channel`, `targets_length_mismatch` (not one target per control),
`delays_length_mismatch` (a `hold` without one delay per control), `target_invalid` (a target the
service cannot build at the rate: a pulse that is not usable, a target that does not fit the
record), `bands_empty`, `target_not_covering` and `target_not_ascending` (an SRS control's
breakpoints, as SRS Shock's), `components_engine_mismatch` and `component_invalid` (an SRS
control's synthesis table, as SRS Shock's), `verdict_needs_srs` (`global` with a `waveform`
target), `plant_unknown`, `law_unknown`, `law_wrong_world`, `law_wrong_target`,
`law_param_unknown`, `law_param_invalid`, `band_cuts_velocity_change` (a classical control's
target cut to the band keeps less than half of its velocity-change margin; the message gives both
figures).

### Running a MIMO Shock Test

- **`mimo_shock.arm`** opens the station with every drive row as an output and every control and
  limit row as an input on one clock, asks it for `acquisition.fs_hz`, adopts the rate it
  programs, derives the grid and builds the targets at that rate, and passes the arm gate. Nothing
  is driven. On the simulator the plant must be the table's size (`arm_refused`). An imported law
  without a passed check is refused `law_not_checked` on a hardware station. The station's own
  refusals are Random's (`arm_refused`, `hardware_busy`, ...).
- **`mimo_shock.start`** from `configured` arms first, then plays the identification shot
  (`phase` `sysid`) and estimates the plant, controls by drives, on the pulse grid (the path
  delays are in it). A control whose band-mean coherence is under 0.8, or whose target has under
  90% of its energy on trusted lines, ends the run `held`. It then computes each control's
  reachable floor and holds for review (`review`), or without a review goes on as `proceed` does.
  It answers at once.
- **The unreachable hold.** With `verdict` `all`, a control whose best reachable response (the
  identified plant's least-squares fit to its target on the trusted lines) fails its own judge
  holds the run in `review` before any pulse, with `review.hold` naming `unreachable_target` and
  each control's floor, whatever `identification.review` says. `mimo_shock.proceed` is then
  refused `unreachable_target` (with the floors) unless it carries a non-empty `acknowledge`, the
  operator's own words, which go in the log. With `verdict` `global` the floors are shown and
  nothing holds.
- **`mimo_shock.proceed`** builds the law and seeds it for the first level. Before any pulse
  fires, the seed's predicted response at every control is held to `first_drive_ratio`; a seed
  over it fires nothing (`law_fault`, `first_drive`). In `auto` the ladder then fires; in
  `manual` it waits for `mimo_shock.fire`.
- **Every pulse**, in this order: build and check the drives (each drive's ceiling and abort, the
  predicted response of every control and limit), fire them in one shot, measure every input,
  judge each control, check the measured response against the abort lines, tick the ladder (which
  fixes the next level), watch for divergence, call the law with the next level, and check its
  answer (shape, budget, growth caps, the predicted response at the next level). An answer that is
  not used counts a miss and the last drive is played again at the next level.
- **`mimo_shock.fire`** fires one pulse. Refused `wrong_mode` in `auto`, and `not_yet` (with
  `seconds`) before the time between pulses has passed or while a pulse is in flight.
- **`mimo_shock.stop`** ends the run at the next boundary: the pulse in flight finishes and
  nothing more fires (`operator_stop`). **`mimo_shock.abort`** takes every drive down with the
  station's own stop at once (`operator_abort`). Both reach a fire in flight.
- **When nobody holds control**, firing pauses; a claim within `timeout_s` resumes it, and
  otherwise the run ends `safe_stop`.
- **The run's end** releases the station (and on hardware its lock) before the end is published,
  so a run started the moment the end arrives finds the station free.

### MIMO Shock Run State

```
{"run_id":"ms-0002","state":"running","phase":"waiting","title":"Four-Drive Half-Sine",
 "source":"SIM","station":{"kind":"sim","class":"sim","devices":[],"plant":"shock-linear-4x4"},
 "grid":{"fs_hz":12800,"record_len":8192,"lines":4097,"actual":true},"clock":"shared",
 "target":"waveform","timing":"free","verdict":"all","scaled_aborts":true,"notice":null,
 "mode":"auto",
 "levels_db":[-12,-9,-6,-3,-1.5,0],"level_db":-6,"rung":2,
 "pulses":{"fired":5,"passed":3,"at_level":1,"of_level":3},
 "next_pulse_s":0.6,"elapsed_s":14.2,
 "identification":{...},"review":{...},"last_pulse":{row},
 "law":{"id":"inverse","name":"inverse","version":"0.1.0","sha256":null,"params":{...},
        "misses":0},
 "end":null}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `armed` | `armed` | Station open and gated; nothing driven yet |
| `running` | `sysid`, `waiting`, `firing` | Identification; waiting out the time to the next pulse (or for `mimo_shock.fire`); a pulse playing |
| `review` | `awaiting_review` | Identification done; nothing fires until `mimo_shock.proceed` |
| `stopping` | `stopping` | The pulse in flight finishes; nothing more fires |
| `finished` | `stopped`, `aborted`, `held` | Ended; `end` says how |

- `station.plant` is the simulator's plant (null on NI). `target`, `timing`, `verdict` and
  `scaled_aborts` are the configuration's. `notice` is null while scaled aborts are on; with them
  off it is a standing sentence saying so, which a screen shows with the run.
- `level_db` is the level the next pulse fires at (the last pulse's once finished; null before the
  law is seeded); `rung` its place in `levels_db` (null when a halved step left the table's
  levels). `pulses.passed` counts the pulses the joint verdict passed; `at_level` the pulses fired
  at this level; `of_level` the most this level may fire without a pass.
- `next_pulse_s` as Classical Shock's.
- `identification` is null until identification is done:

```
{"realizations":64,"drive_vrms":[0.05,0.05,0.05,0.05],"drive_vpk":[0.18,0.18,0.18,0.18],
 "coherence_mean":[0.998,0.997,0.998,0.996],"coverage":[0.999,0.999,0.998,0.999],
 "trusted_lines":1596,"band_lines":1596,"condition_max":3.1,"coherence_floor":0.5,
 "passed":true}
```

  Each drive's rms and peak in the shot, volts; each control's band-mean multiple coherence and
  the share of its target's energy on trusted lines; the trusted and in-band lines; the largest
  condition number of the plant on a trusted line; `passed` false when it held the run.
- `review` is null until identification is done, then each control's reachable floor and the
  hold, and after the seed each control's predicted peak:

```
{"controls":[{"channel":1,"reachable":0.9998,"floor_db":-31.2,"reachable_ok":true,
              "predicted_peak_ratio":1.01}, ...],
 "hold":null,"acknowledged":null}
```

  `reachable` is the share of the control's target energy the drives can reach at all;
  `floor_db` the waveform error (`waveform`, at least -120 dB, the precision it is computed to)
  or the largest SRS deviation (`srs`) of its best reachable response, dB; `reachable_ok` whether that response passes the control's own judge.
  `predicted_peak_ratio` is the seed's predicted peak over the target's at the first level (null
  before the seed). `hold` is null or `{"reason":"unreachable_target","message"}`;
  `acknowledged` the operator's words once given.
- `last_pulse` is the last pulse's row (below); `law` the run's law, its parameter values in force
  and its unusable calls in a row now.
- `end` is null until the run finishes, then `{"outcome","reason","message","at_s","level_db",
  "pulse"}`: `pulse` the pulse it ended at (null before any).

| `outcome` | How the run ended |
|---|---|
| `completed` | Full level passed its pulses |
| `operator_stop` | `mimo_shock.stop` |
| `operator_abort` | `mimo_shock.abort` |
| `safe_stop` | The controlled stop: nobody held control (`reason` `orphan`), the service shut down (`shutdown`), a control over its level-scaled line with scaled aborts on (`control_abort_scaled`), the divergence watch (`divergence`), a predicted response over a scaled line before a fire (`predicted_abort`), or the pulse cap (`max_pulses`) |
| `aborted` | A protection tripped on what was driven or measured: `drive_abort`, `control_abort_absolute` (a control over its full-level line), `limit_abort`, `kinematics` |
| `not_converged` | `max_pulses_per_level` pulses at one level without a joint pass (`not_converged`) |
| `held` | Identification held the run before any pulse: `low_coherence` or `reference_out_of_band` |
| `law_fault` | The law gave no usable drives `law_misses` times in a row, panicked, could not be built, or its seed was refused; `reason` is the last miss's: `error`, `panicked`, `over_budget`, `wrong_length`, `non_finite`, `undeclared_value`, `too_many_values`, `peak_rise`, `bin_rise`, `predicted_peak`, `first_drive`, `target_not_accepted` |
| `fault` | The run or the station failed; `message` is the controller's words |

### MIMO Shock Pulses

Every pulse fired has a row; `mimo_shock.pulses` and `mimo_shock.pulse` are `shock.pulses` and
`shock.pulse` for MIMO Shock. `mimo_shock.pulse`'s views are, per control, **Pulse** *n* (the
target at the level fired, moved by the control's delay, its tolerance band for a `waveform`
target, and the measured response, EU, against time) and for an `srs` target **SRS** *n* (the
measured SRS, the target at the level fired and its tolerance band), then **Drives** (every
drive as played, one trace each, `Drive` *n*). Every per-channel view carries `channel`, its
table index.

```
{"index":4,"scheduled_db":-6,"level_db":-6,"passed":false,"at_s":11.2,
 "drives":[{"channel":5,"vpk":0.29,"limit_scale":1}, ...],
 "controls":[{"channel":1,"passed":false,"reasons":["tolerance"],"error_db":-21.4,
              "over_test_db":0.4,"delay_s":0.00031,"peak":10.0,"peak_ratio":1.047,
              "scaled_margin_db":-1.88,"absolute_margin_db":-7.88,
              "timing_ok":true,"polarity_ok":true,"duration_ratio":null,"duration_ok":true,
              "energy_db":0.21,"rms_duration_ratio":1.01,"velocity_ips":12.1,
              "displacement_pk_pk_in":0.18,"predicted_peak_ratio":0.99,"srs":null}, ...],
 "over_test_db":0.4,"global_srs_error_db":null,"arrival_spread_s":0.0004,
 "warnings":[],"step_note":null,
 "law":{"used":true,"miss":null,"call_ms":3.1,"delays_s":null,
        "values":[{"key":"waveform_error","value":0.031}, ...]}}
```

- `scheduled_db` is the level the ladder asked for and `level_db` the level fired (the same: a
  drive the ceiling scales is reported in its `limit_scale`, not as a level). `passed` is the joint
  verdict the ladder was fed. `at_s` is the pulse's start, seconds from arm.
- `drives`: each drive's peak as played, volts, and the scale its ceiling applied (1 when none).
- `controls`: each control's judgement. `reasons` names why it failed: `tolerance` (outside its
  standard's tolerance, or its SRS outside the tolerance band), `timing` (its delay outside its
  window), `polarity` (upside down) and `duration` (an `srs` control's effective duration outside
  0.8 to 1.2 times its synthesized waveform's); empty when it passed. `error_db` is the waveform
  error after the control's delay is removed (`waveform`) or the largest SRS deviation (`srs`);
  `over_test_db` the worst signed error, positive an over-test (the peak over the target's peak,
  or the SRS deviation above the target); `delay_s` the control's delay; `peak` (EU) and
  `peak_ratio` (over the target's peak at the level); `scaled_margin_db` and `absolute_margin_db`
  the measured peak against the control's scaled and absolute abort lines, dB, positive over the
  line; `duration_ratio` the effective duration over
  the target's (`srs`, else null); `energy_db` and `rms_duration_ratio` the response's energy and
  rms duration against the target's, reported (a stated tolerance of 1 dB and 10% gives a
  warning, never a failure); `velocity_ips` and `displacement_pk_pk_in` through the pure
  integrator; `predicted_peak_ratio` the prediction before the fire; `srs` for an `srs` target
  `{"worst_err_db","worst_band_hz","bands","passed_bands"}`, else null.
- `over_test_db` is the worst over every control; `global_srs_error_db` the energy-weighted SRS
  error over every control (MIL-STD-810H Method 527.2 Annex C; the joint verdict with `verdict`
  `global`, else shown only; null for a `waveform` target); `arrival_spread_s` the latest control
  delay less the earliest.
- `warnings`: what is reported beside the verdict: every control that crossed its scaled abort
  line (always, whatever `scaled_aborts` says), an arrival spread over the window, energy or rms
  duration outside its stated tolerance.
- `step_note`: when the ladder stepped up after this pulse by less than the table's step (the
  first pulses' trend predicted an over-test above the upper tolerance at the table's next
  level), `{"from_db","planned_db","taken_db","predicted_planned_db","predicted_taken_db",
  "tolerance_db"}`; else null.
- `law` is the call that produced the drives this pulse played: `used` false and `miss` the
  reason when the drives in force were played again after a miss; `call_ms` its time; `delays_s`
  the delay it expected at each control, when it gave one; `values` its named values.

### MIMO Shock Display

Event topics:

- `mimo_shock.state` (text): the state object on every change.
- `mimo_shock.log` (text): new log entries, as Random's.
- `mimo_shock.live` (binary, to every connection, 5 times a second from arm until the run has
  finished, then once more): `{"run_id","source","state","phase","mode","level_db","rung",
  "pulses","next_pulse_s"}`.
- `mimo_shock.pulse` (binary, to every connection, once per pulse): `{"run_id","pulse":row,
  "views"}` with the views of `mimo_shock.pulse` at 1024 points.
- `mimo_shock.data` (binary, to one subscribed connection): `mimo_shock.live`'s fields and `plots`,
  once per pulse, once when identification ends, and at once on subscribe, at most `rate_hz`
  frames a second.

`mimo_shock.subscribe` takes Classical Shock's form (`{"rate_hz","plots":[plot...]}` or
`{"default":true}`); a plot is `{"id","quantity","points","channel"}`, `points` for the time
histories only (default 1024, 64 to 8192), `channel` (optional) a control's table index for the
per-control quantities. The result is `{"default","rate_hz","plots","quantities"}`, every default
filled in. In `mimo_shock.data` each plot comes back as `{"id","quantity","views"}`.

| `quantity` | Views | What they show |
|---|---|---|
| `pulse` | **Pulse** *n*, one per control or the plot's `channel` | As `mimo_shock.pulse`'s |
| `srs` | **SRS** *n*, one per control or the plot's `channel` (an `srs` target) | As `mimo_shock.pulse`'s |
| `drives` | **Drives** | Every drive as played, V |
| `verdicts` | one `table` | Each control's judgement of the latest pulse |
| `ladder` | one `table` | The levels, the pulses fired and passed at each, the level in force |
| `identification` | **Identification Gain** *n*, **Coherence** | `abs(H)` from every drive to the plot's `channel` (default the first control), EU per volt, one trace per drive (`Drive` *n*); every control's multiple coherence (`Control` *n*) with the floor |
| `review` | one `table` | Each control's reachable share, floor and whether it is reachable, and the seed's predicted peak |
| `law` | one `reading` | The law's named values on its last call |
| `readings` | one `reading` | The run's numbers |
| `pulses` | one `table` | The pulse rows |

A plot of the latest pulse is empty until a pulse has fired, and `identification` and `review`
until identification has ended.

## Sine

A Sine test drives one tone and closes the loop on its amplitude every control tick. The
service identifies the system with a low-level noise burst, checks the identification's
coherence, and engages at the first level of the schedule; each tick the control law decides
the tone's amplitude from the measured response, and the service caps it, applies the limit
channels' notch and the drive ceiling, and judges the response against the profile's alarm
and abort bands at the present frequency. The frequency, the sweep and its direction, the
tone's phase and the level schedule are the service's, never the law's. The drive is in volts
peak on hardware. The run is one test like any other: a Sine run, an SRS shock run, a
classical shock run, a Random run, a MIMO run, a TWR run, an Explorer run, a Recorder run and a
`sim.start` run exclude each other, each refused `run_active` while another is live. **Any connected screen may stop or abort**
(`sine.stop`, `sine.abort`); `sine.configure` (other than a dry run), `arm`, `start`,
`proceed`, `level`, `mode`, `hold`, `resume`, `reverse`, `jog`, `goto`, `adjust`, `reset` and
`clear` need control; everything that only reads or only computes is open to every
connection. While the service is shutting down, `sine.arm`, `start`, `proceed`, `level`,
`mode`, `resume`, `reverse`, `jog`, `goto` and `adjust` are refused `shutting_down`;
`sine.hold`, `stop` and `abort` are not.

| Method | Params | From | Result |
|---|---|---|---|
| `sine.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived","views"}` |
| `sine.config` | `{}` | any | `{"run_id","config","derived","views"}` |
| `sine.profile.import` | `{"text"}` or `{"path"}` | any | `{"profile","errors","warnings"}` |
| `sine.profile.validate` | `{"profile","band_hz","feasibility"}` (`band_hz` and `feasibility` optional) | any | `{"errors","warnings","crossovers","kinematics","views"}` |
| `sine.arm` | `{"override_ack"}` (optional) | `configured`, `blocked` | state |
| `sine.start` | `{}` | `configured`, `armed` | state |
| `sine.proceed` | `{}` | `review` | state |
| `sine.level` | `{"delta_db"}` or `{"to_db"}` | `running`, in `manual` | `{"level_db"}` |
| `sine.mode` | `{"mode":"auto"\|"manual"}` | any configured state | `{"mode"}` |
| `sine.hold` | `{}` | `running` | state |
| `sine.resume` | `{}` | `running`, held | state |
| `sine.reverse` | `{}` | `running`, a sweep | state |
| `sine.jog` | `{"delta_hz"}` | `running`, a sweep | `{"frequency_hz"}` |
| `sine.goto` | `{"hz"}` | `running`, a sweep | `{"frequency_hz"}` |
| `sine.adjust` | `{"key","value"}` | `running`, once the law is built (after `sine.proceed`) | `{"key","value"}` |
| `sine.stop` | `{}` | any | state |
| `sine.abort` | `{}` | any | state |
| `sine.reset` | `{}` | any but live | state |
| `sine.clear` | `{}` | any but live | state |
| `sine.state` | `{}` | any | state |
| `sine.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `sine.subscribe` | see "Sine Display" | any | as there |

"state" in the Result column is the `sine.state` object below. A run is live while it is armed,
running, holding for review or stopping. `sine.configure` replaces a run that is not live;
`sine.reset` configures the present run's configuration again on the present table and
station; `sine.clear` forgets it (`unconfigured`). A result that carries views travels as a
binary frame.

### Sine Configuration

```
{"title":"Sine 1 g, 20 to 2000 Hz",
 "profile":{"label":"1 g","rows":[{"freq_hz":20,"quantity":"disp_pp_in","value":0.0097},
                                  {"freq_hz":45,"quantity":"accel_g","value":1,
                                   "alarm_db":[1,1],"abort_db":[2,2]},
                                  {"freq_hz":2000,"quantity":"accel_g","value":1}],
            "alarm_db":[1.5,1.5],"abort_db":[3,3]},
 "test":{"kind":"sweep","band_hz":[20,2000],"rate":{"law":"log","oct_per_min":2},
         "start_hz":20,"direction":"up","end":{"sweeps":4,"duration_s":null}},
 "acquisition":{"fs_hz":null,"tick_ms":10},
 "schedule":{"mode":"auto","levels_db":[-12,-6,0],"start_db":-12,
             "per_level":{"sweeps":1},"full_level":{"sweeps":2}},
 "identification":{"level_db":-20,"gate_coherence":0.5,"trust_coherence":0.5,
                   "trust_h_floor_frac":0.05,"review":true},
 "control":{"composite":"weighted_avg","weights":[{"channel":1,"weight":1}],
            "tracking":{"kind":"proportional","pct":10,"max_hz":100},
            "law":{"id":"servo_blend","params":{"blend_w":0.5,"compression":"slow","kp":0.3}},
            "law_budget":0.25,"law_misses":3,"max_step_db":1,"max_rise_db_per_s":240,
            "divergence_s":1,"fault_debounce_ticks":50},
 "limits":{"drive_vpk_ceiling":0.5,"drive_vpk_abort":0.8,"kinematic_monitor":true,
           "channels":[{"channel":3,"shared_name":null,
                        "profile":{"label":"Fixture bound",
                                   "rows":[{"freq_hz":20,"quantity":"accel_g","value":3},
                                           {"freq_hz":2000,"quantity":"accel_g","value":3}],
                                   "alarm_db":[1.5,1.5],"abort_db":[3,3]}}],
           "notch":{"drive_floor":0.01,"smoothing_ticks":3,"release_ramp_db_per_s":20}},
 "cola":{"enabled":false,"volts_pk":0.1,"waveform":"sine"},
 "feasibility":{"shaker":{"name":"Bench shaker",...},
                "moving_mass_lb":{"armature":45,"slip_table":0,"drive_adapter":3,"fixturing":10,
                                  "test_article":12}},
 "operator":"A. Operator","article":"Bracket, serial 7","notes":"first article",
 "specification":{"document":"Test procedure 12","clause":"4.2"},"record":{"streams":true},
 "metadata":[{"key":"run_number","label":"Run Number","value":"7"}]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `profile` | (required) | The control profile: `rows`, at least two, each `{"freq_hz","quantity","value"}` with optional `alarm_db` and `abort_db`, frequencies above 0 and strictly ascending, values above 0. `quantity` is `accel_g` (g peak), `velocity_ips` (in/s peak) or `disp_pp_in` (in peak to peak). A row governs up to the next row; neighbours of one quantity are joined on log-log axes, and a change of quantity holds the left row's value (a jump of more than 0.5 dB at the join is a warning). `alarm_db` and `abort_db` are `[below, above]` in positive dB (defaults `[1.5,1.5]` and `[3,3]`); a row's own bands hold from that row to the next. `label` is optional. The profile must cover the test's band |
| `test.kind` | (required) | `sweep`, `dwell`, `step` or `rstd` (below) |
| `test` (`sweep`) | | `band_hz` `[lo, hi]` (above 0, under 0.45 times the rate); `rate` `{"law":"log","oct_per_min"}` or `{"law":"linear","hz_per_s"}`; `start_hz` (default `lo`); `direction` `up` (the default) or `down`, the first leg; `end` `{"sweeps","duration_s"}`: the run ends after this many one-way sweeps, or seconds of test time, or whichever comes first with both, counted over the whole run from engage at every level |
| `test` (`dwell`) | | `freq_hz`, and `end` `{"seconds"}` or `{"cycles"}` (a whole number) |
| `test` (`step`) | | `band_hz`, `steps_per_octave` (above 0), `spacing` `log` (the default) or `linear`, and `dwell` at each step: `{"seconds"}`, `{"cycles"}`, or null for a dwell computed per step |
| `test` (`rstd`) | | Resonance search, track and dwell: `band_hz`, the search's `level_db` and `rate`, `tracking_bw_hz`, `settle_k` (default 5: settle for `settle_k / tracking_bw_hz` s), `dwell_mode` `fixed` or `phase_tracked`, and `dwell` `{"seconds"}` or `{"cycles"}` |
| `acquisition.fs_hz` | null: 16384 | The rate requested of the station, 1000 to 204800 Hz; the run reports the rate the station adopted (`grid`, `actual` true once armed) |
| `acquisition.tick_ms` | 10 | The control tick, ms, converted to the nearest whole frame (at least one) at the rate the station adopts: 2 to 1000 for a built-in law, 8 to 1000 for a law file |
| `schedule.mode` | `auto` | `auto` or `manual`, as Random's |
| `schedule.levels_db` | `[-12,-9,-6,-3,0]` | Levels relative to the profile, dB, ascending, each from -60 to 12; the last is full level |
| `schedule.start_db` | the first level | The level the run engages at; a level of the schedule |
| `schedule.per_level`, `full_level` | `{"sweeps":1}` each | In `auto`, how long each level but the last is held before the ladder moves on, and how long full level is held before the ladder counts as done: `{"sweeps"}` (one-way sweeps, at least 1; a sweep test only) or `{"seconds"}`. `per_level` is checked only when the run starts below the last level: with one level, or a start at the last, it is never used (a dwell may then leave it at its default). The run itself ends at the test's `end`; an `end` that would come before the ladder has held full level is refused (`end_before_full_level`) |
| `identification.level_db` | -20 | The noise burst's level under the full-level profile, dB (-60 to 0) |
| `identification.gate_coherence` | 0.5 | The band coherence the identification must reach; under it the run is held (`held`), 0 to 1 |
| `identification.trust_coherence`, `trust_h_floor_frac` | 0.5, 0.05 | Where the identification is trusted: a frequency whose coherence is under `trust_coherence`, or whose gain is under `trust_h_floor_frac` of the in-band peak, has no prediction there (the law is told it is absent) |
| `identification.review` | true | Hold for the operator after the gate passes (`review`) |
| `control.composite`, `weights` | `weighted_avg`, 1 each | How the control rows combine, as Random's: `weighted_avg`, `min` or `max`, each control row's weight above 0 |
| `control.tracking` | `{"kind":"proportional","pct":10,"max_hz":100}` | The tracking filter's bandwidth: `{"kind":"fixed","hz"}` or `{"kind":"proportional","pct","max_hz"}` (a percentage of the frequency, above 0 and at most 100, capped at `max_hz`). A sweep too fast for it is refused or warned (`tracking_lag`) |
| `control.law` | `{"id":"servo_blend"}` | The control law, by its catalog `id` (see "Control Laws", world `sine`), and values for the parameters its manifest declares (`params` by key; defaults fill the rest). The built-in `servo_blend` is the amplitude servo and the feedforward blend: `blend_w` (0 to 1, default 0.5, live), `compression` (`slow`, `fast` or `custom`, default `slow`), `ki` and `max_rate_db_per_s` (with `custom`), `kp` (default 0.3) |
| `control.law_budget` | 0.25 | The fraction of the tick one law call may take (above 0, at most 1) |
| `control.law_misses` | 3 | Unusable law calls in a row that stop the run (1 to 100) |
| `control.max_step_db` | 1 | The most the drive may change in one tick, dB either way (1 to 3) |
| `control.max_rise_db_per_s` | 240 | The most the drive may rise in a second, dB (above 0, at most 10000) |
| `control.divergence_s` | 1 | Seconds the change cap may bind without a break before the run aborts (`divergence`), above 0, at most 60 |
| `control.fault_debounce_ticks` | 50 | Control ticks the control point may read far under its target before the run aborts (`servo_fault`), at most 100000; 0 turns it off |
| `limits.drive_vpk_ceiling` | (required) | The drive ceiling, volts peak (above 0, at most 10). The drive is held at or under it |
| `limits.drive_vpk_abort` | (required) | The drive abort, volts peak, at least the ceiling and at most 10. A drive at or over it aborts the run (`drive_vpk`) |
| `limits.kinematic_monitor` | true | With `feasibility`, abort when the response's velocity or displacement passes the shaker's rating (`kinematic`) |
| `limits.channels` | none | Limit channels: each a table row's `channel` (any enabled input but a control row) and a `profile` in the profile's form, the bound at each frequency, covering the test's band; `shared_name` names a profile several channels share. Where a channel would pass its bound the service holds the drive down (the notch), and a channel outside its own abort band aborts (`limit_channel`) |
| `limits.notch` | `{"drive_floor":0.01,"smoothing_ticks":3,"release_ramp_db_per_s":20}` | The notch: the drive under which a channel's gain is not estimated (a fraction of full drive, above 0 and under 1), the ticks its estimate is smoothed over (at least 1), and how fast the drive may come back once the notch releases, dB/s |
| `cola` | off | A second, constant-amplitude reference output locked to the tone: `enabled`, `volts_pk` (default 0.1, above 0 and at most 10), `waveform` `sine` (the default), `square` or `dc`. It needs a second drive row and a station with two outputs (the simulator has one): `cola_needs_two_outputs` |
| `feasibility` | null | Random's shaker record and moving mass (no crest factor: a sine's is 1). Checked at configure and at arm against the profile's peaks; a record that does not parse is refused `shaker_record_invalid`. Without it, feasibility is not evaluated, the kinematic monitor is off, and the run says so |
| `operator`, `article`, `notes`, `specification`, `record` | empty, null, `{"streams":true}` | Random's, for the run's record (see Sine Run Folders): the operator's words, the document and clause the test is run to, and whether the record keeps the inputs and the drive as streams. None of them changes how the run drives |
| `metadata` | the default fields | Random's: `{"key","label","value"}`, `label` the key when absent |
| `operator`, `article`, `notes` | empty | The operator's words for the run's record, as Random's. Empty `operator` and `article` take the `metadata` fields `operator` and `test_article` |
| `specification` | null | `{"document","clause"}`, as Random's: what the run was made to |

Sine takes no `capture`: every run is recorded (see Sine Run Folders), and a configuration that
gives one is refused `bad_params` at `config.capture`.

The control channels are the table's enabled `control` rows, each with its weight (1 unless
`control.weights` names it); the drive is its first enabled `drive` row (with COLA, the second
is the reference output). A configuration whose shape is
wrong is refused `bad_params` naming the field in `detail.param` as the request's own path
(`config.test.kind`); its content errors are refused `config_invalid` with the items,
located by `field`, the path inside the configuration (`limits.drive_vpk_abort`, with no
`config.` before it), or by `row` for a control profile row;
`dry_run: true` checks without replacing anything and needs no control. RSTD is accepted on the
wire; the shell does not offer it.

**Result.** `state` is the `sine.state` object (`configured`). `derived`:

```
{"band_hz":[20,2000],"grid":{"fs_hz":16384,"tick_frames":164,"tick_s":0.0100098,"actual":false},
 "profile":{"crossovers":[{"freq_hz":44.9,"from":"disp_pp_in","to":"accel_g"}],
            "peak_accel_g":1,"peak_velocity_ips":1.37,"peak_disp_pp_in":0.0097},
 "test_s":797.3,"identification_s":128.0,"call_budget_s":0.0025,
 "stop_ramp_db_per_s":120,"safe_stop_s":0.95,
 "tracking":{"verdict":"ok","ratio":0.04,"at_hz":null,"message":null},
 "law":{"id":"servo_blend","name":"Servo blend","version":"0.1.0","sha256":null,
        "live":["blend_w"]},
 "feasibility":{"verdict":"pass","message":"..."}}
```

- `grid`: the requested rate and the tick in whole frames and in seconds (7 decimals) at it;
  `actual` is true once the run has armed and both are the station's.
- `profile`: where the profile changes quantity (each pair of neighbouring rows of different
  quantities, at the frequency where both give the same acceleration), and its peaks over the
  band (g, in/s, in pk-pk), each to 4 significant digits.
- `test_s`: the test's own length in test time (holds excluded), to 0.1 s: the sweeps (the
  first leg from `start_hz`) or the duration, whichever ends first; a dwell's seconds or
  cycles; a step test's dwells summed. Null in `manual`, for a step test whose dwells are
  computed per step, and for a resonance search. `identification_s` is the noise burst's
  length at the requested rate (its bins no wider than the band's low edge over 200, at least
  16384 points, eight segments), to 0.1 s.
- `call_budget_s`: one law call's budget, `law_budget` times the tick (7 decimals).
- `stop_ramp_db_per_s` and `safe_stop_s`: the controlled stop's ramp, which is the service's and
  not configurable, and its length from the drive ceiling to 120 dB under one volt, in station
  time (3 decimals). A ceiling whose stop is longer than the service waits for a stop is
  refused (`safe_stop_budget`).
- `tracking`: the sweep against the tracking filter, on the drive-to-response lag the run can
  expect before the station measures it (one block at the requested rate): `ok`, `advisory`
  (the readout is measurably low at `at_hz`; a warning) or `refused` (an error item);
  `ratio` the worst lag-induced offset over the filter's bandwidth, to 3 significant digits.
  The arm checks it again on the station's own estimate, and the identification on the lag it
  measures, and either refuses the run.
- `law`: the run's law and the keys of its live parameters.

`views` are **Profile** (`spectrum`, x `Frequency` Hz log, y `Acceleration` g log: series
`Profile` (`reference`), `Alarm high` and `Alarm low` (`tolerance`), `Abort high` and
`Abort low` (`abort`), at full level), **Velocity** (y `Velocity` in/s log) and
**Displacement** (y `Displacement` in pk-pk log), each with the one series `Profile`
(`reference`); every series is a line on 256 log-spaced points across the band and every row
frequency inside it, and every view has `link` `frequency`. They are what
`sine.profile.validate` gives over the test's band.

**Profiles.** `sine.profile.import` reads rows from `text` (pasted rows) or from the file at
`path` (a full path on the computer that runs the test, at most 4 MiB), one of the two (both or
neither is `bad_params`): a header row naming the frequency column (`freq_hz`, `freq`,
`frequency` or `hz`) and the quantity columns (the wire's own names `accel_g`, `velocity_ips`
and `disp_pp_in`, or `accel`, `g` or `g_pk`; `velocity`, `vel` or `ips`; `displacement`, `disp`
or `in_pk_pk`), with exactly one quantity given per row, and
optionally `alarm` and `abort` (symmetric dB; `alarm-` and `alarm+` for the two sides, or
`alarm_pct`) and a `units` column, which is read and ignored (`units_column_ignored`); case,
spaces, underscores and a unit in parentheses do not matter. A two-column file without a header
is frequency and g peak. Tab, comma, semicolon or space separated, `#` and `//` comments
allowed. It returns the `profile` a
configuration takes, every field written, or null with error items: `profile_file` and
`quantity_ambiguous` at the data `row` they name (from 0, after the header and comments; the
words say what is wrong there), `profile_file` at `field` `text` or `path` when the input as a
whole cannot be read, and the profile's own items located by `row`; a relative path is
`bad_params`. `sine.profile.validate` checks a profile on its own, its items located by
`field` `profile.rows[<i>]` or `profile.<key>`, over `band_hz` when given (a band the profile
does not cover is `profile_not_covering` at `band_hz`), and returns its items, its crossovers,
its `kinematics` (`{"peak_accel_g","peak_velocity_ips","peak_disp_pp_in","verdict","message"}`
over the band, the verdict against the shaker's ratings when `feasibility` is given and
`not_evaluated` without it) and the three views; with errors, `kinematics` is null and
`views` empty. Neither needs control or changes the run. A profile drawn by the service is a
computation, never a measurement.

Item reasons (Sine): errors `out_of_range`, `profile_too_short`, `frequency_not_positive`,
`frequency_not_ascending`, `value_not_positive`, `quantity_ambiguous` (an imported row with
more than one quantity), `profile_not_covering` (the profile does not cover the test's band),
`band_above_grid` (a band edge at or above 0.45 times the requested rate), `levels_empty`,
`levels_not_ascending`, `start_not_a_level`, `end_before_full_level`, `tick_out_of_range`,
`abort_below_ceiling`, `no_control_channel`, `weight_unknown_channel`, `no_drive_channel`, `limit_unknown_channel`,
`limit_on_control_channel`, `cola_needs_two_outputs`, `safe_stop_budget` (the stop could not
reach zero inside the service's own stop budget), `tracking_lag` (refused), `law_unknown`,
`law_wrong_world`, `law_param_unknown`, `law_param_invalid`; `profile_file` from the import;
warnings `join_discontinuity`, `alarm_wider_than_abort`, `tracking_lag` (advisory),
`units_column_ignored` (from the import).

### Running a Sine Test

- **`sine.arm`** checks feasibility (when given), opens the station, adopts its rate, converts
  the tick to the nearest whole frame at that rate, narrows the band to under 0.45 times it (logged) and
  passes the arm gate. Nothing is driven. Refusals as `random.arm`'s, with `law_not_checked` on
  a hardware station for an imported law without a passed check for its exact file.
- **`sine.start`** from `configured` arms first. It runs identification (`phase` `sysid`): a
  white-noise burst at `identification.level_db`, measuring the gain and coherence to each
  control row and the drive-to-response lag, then the coherence gate (`coherence_gate`). Under
  `gate_coherence` the run ends `held`; otherwise it holds for review (`review`) or, without
  one, goes on as `sine.proceed` does. It answers at once.
- **`sine.proceed`** builds the law from the identification (a law that cannot be built ends the
  run `law_fault` before the tone starts) and engages at `schedule.start_db`. The first drive is
  at most the identification's prediction at the start frequency and level plus 6 dB, and never
  over the ceiling; without a trusted prediction there, it is at most 0.05 times the start
  level's scale. The settle window then keeps the band, servo-fault and kinematic aborts off
  while the response comes onto the band.
- **Each tick** the service measures each control row's tracked amplitude and phase, composites
  them, observes the limit channels, calls the law, then caps the drive: at most `max_step_db`
  from the drive in force, at most `max_rise_db_per_s` up, under the notch's ceiling and the
  drive ceiling, and aborts on the drive abort. A late, failed, panicked or refused call keeps
  the drive in force; a usable call resets the count; `law_misses` in a row end the run with the
  controlled stop (`law_fault`). While a resonance search maps the plant the law is called and
  its drive is not used.
- **`sine.level`** and **`sine.mode`** are `random.level` and `random.mode` for Sine, with the
  same refusals (`manual_mode_required`, `not_a_rung`): `delta_db` steps the scheduled level by
  that many dB, held between the schedule's lowest and highest levels, `to_db` moves it to a
  level of the schedule, and the result is the scheduled level once the service has taken the
  change (a step past either end answers with the end it stopped at). Every level change
  re-opens the settle window (see `sine.goto`). A level change is a change of target: the law
  follows it within the service's caps.
- **`sine.hold`** freezes the test's progress: the sweep, dwell and level clocks stop and the
  tone stays at its frequency and level, still under control (`held` true). **`sine.resume`**
  starts them again. Neither is a run state, and neither releases a review or a held gate.
- The run verbs (`hold`, `resume`, `reverse`, `jog`, `goto`, `level`, `adjust`) are taken
  between the service's steps of a quarter second of station time, and answer once the change
  has been taken.
- **`sine.reverse`** turns the sweep at the next tick. **`sine.jog`** moves the frequency by
  `delta_hz`, stopping at the band's edge, and **`sine.goto`** moves it to `hz` (refused
  `out_of_band` outside the band); each answers with the frequency applied, and the tone moves
  without a break in phase. All three are sweeps only, refused `not_a_sweep` in another test.
  A jog and a go to re-open the settle window, as a level change does: for 40 dB at the
  compression's rate limit plus 0.3 s (0.97 s at `fast`, 2.3 s at `slow`), the band, servo-fault and kinematic
  aborts are off and only the drive ceiling and the drive abort bound the drive, while the
  response comes onto the band at the new frequency.
- **`sine.adjust`** changes a live parameter of the run's law: a number parameter the law
  declares live, within its declared range. It applies from the next tick and is logged (`law`).
  Refused `adjust_refused`, with `detail.why` `unknown_key`, `not_live`, `out_of_range` or
  `law_refused` (the law's own reason in `message`).
- **`sine.stop`** is the controlled stop: the drive, and the reference output with it, ramps
  down at 120 dB a second of station time from the drive in force to 120 dB under one volt,
  then to zero (state `stopping`), and the run ends `operator_stop` once every output is at
  zero. From `armed` or `review` it ends at once. It answers at once.
- **`sine.abort`** ends the run within one batch of ticks without the ramp; the station takes
  the drive down with its own stop. The outcome is `operator_abort`.
- **When nobody holds control** the run goes on; after `timeout_s` it ends `safe_stop` with the
  controlled stop, as Random's. While the service shuts down it ends the same way.

**Time is the station's.** `elapsed_s`, `at_level_s`, the live frames' `t_s` and `end.at_s`
count the station's own samples at the rate it adopted (frames played over `grid.fs_hz`),
never the service's clock, so a sweep rate, a hold or a ramp judged from them is judged on the
station's time. The stop ramp is defined the same way: 120 dB per second of station time,
evaluated per sample, whatever the service's thread is doing, and `end.ramp_s` is its length
on the station's clock. The safe stop's countdown (`orphan_s`) is the only number timed by the
service's clock: it measures how long nobody has held control.

### Sine Run State

```
{"run_id":"sine-0003","state":"running","phase":"engaged","title":"Sine 1 g, 20 to 2000 Hz",
 "source":"SIM","station":{"kind":"sim","class":"sim","devices":[]},
 "grid":{"fs_hz":16384,"tick_frames":164,"tick_s":0.0100098,"actual":true},"clock":"shared",
 "test":"sweep","mode":"auto","levels_db":[-12,-6,0],"level_db":-6,"rung":1,
 "frequency_hz":970.2,"direction":"down","sweeps":{"done":0,"of":1},"held":false,
 "elapsed_s":230.6,"at_level_s":31.3,"remaining_s":566.7,"alarm":false,"notch":false,
 "identification":{"level_db":-20,"band_coherence":0.98,"gate_coherence":0.5,"lag_frames":0,
                   "trusted_frac":0.97,"passed":true},
 "law":{"id":"servo_blend","name":"Servo blend","version":"0.1.0","sha256":null,"misses":0,
        "capped_ticks":0,"live":{"blend_w":0.5}},
 "feasibility":{"verdict":"pass","message":"..."},"end":null,
 "record":{"folder":"/home/operator/Hertzforge_Data/20261101-142233_sine_sine-0003",
           "streams":"on","problem":null}}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `blocked` | `feasibility_blocked` | Feasibility exceeded; arming needs an acknowledgement |
| `armed` | `armed` | Station open and gated; nothing driven yet |
| `running` | `sysid`, `coherence_gate`, `engaged`, `full_level` | Identification, its gate, the schedule, full level |
| `review` | `awaiting_review` | The gate passed; nothing drives until `sine.proceed` |
| `stopping` | `ramping_down` | A controlled stop or a safe stop is ramping the drive down |
| `finished` | `stopped`, `aborted`, `coherence_hold` | Ended; `end` says how |

- `test` is the test's kind; `mode`, `levels_db`, `level_db` (the scheduled level) and `rung`
  as Random's and classical shock's. `phase` turns `full_level` once the ladder is at its last
  level, in `manual` as in `auto` (Random's meaning), whatever the ladder's own count; the run
  itself ends at the test's `end`.
- `frequency_hz` is the tone's frequency now; `direction` `up` or `down` in a sweep (null
  otherwise); `sweeps` the one-way sweeps done at this level and, in `auto` with a sweep count,
  of it (null outside a sweep); `held` true while `sine.hold` holds the progress.
- `elapsed_s` is test time since the loop engaged (holds excluded), `at_level_s` the time at
  this level, `remaining_s` the test time left (null in `manual` or when not known); all station
  time.
- `alarm` is true while the control point is outside its alarm band at the present frequency;
  `notch` true while a limit channel holds the drive down.
- `identification` is null until the gate has run: its level, the band coherence against the
  gate, the drive-to-response lag it measured, and the fraction of its grid inside the band it
  trusts.
- `law` is the run's law, its unusable calls in a row now, the ticks a cap has bound, and the
  live parameters' values in force.
- `end` is null until the run finishes, then `{"outcome","message","at_s","level_db","at_hz",
  "ramp_s","trip"}`: `at_s` station time from engage, `level_db` and `at_hz` where the end began,
  `ramp_s` the controlled stop's length on the station's clock (null for an abort; a
  `divergence` trip and a `law_fault` take the controlled stop, so theirs is a number, from the
  trip's tick).
- `record` is Random's: absent until the run arms, then the run folder, whether it holds the
  streams, and the latest problem writing it.

| `outcome` | How the run ended |
|---|---|
| `completed` | The schedule finished; the drive ramped down with the controlled stop |
| `operator_stop`, `operator_abort`, `safe_stop` | As Random's |
| `aborted` | A protection tripped; `trip` says which |
| `held` | The coherence gate held: the band coherence stayed under `gate_coherence`, so the run never drove at test level |
| `law_fault` | The law gave no usable drive `law_misses` times in a row, or could not be built; the drive in force was held through the misses, then the run took its controlled stop. `trip` says why |
| `fault` | The loop or the station failed; `message` is the controller's words |

`trip` is null unless `outcome` is `aborted` or `law_fault`; it has `kind`, `at_hz`,
`at_level_db`, `at_tick` and its kind's facts:

| `kind` | Facts |
|---|---|
| `abort_band` | `side` (`high` or `low`), `measured_g`, `limit_g` |
| `kinematic` | `axis` (`velocity` or `displacement`), `value`, `limit` |
| `limit_channel` | `channel`, `measured`, `limit` |
| `drive_vpk` | `vpk`, `abort_vpk` |
| `servo_fault` | `consecutive_ticks` |
| `watchdog` | `channel`, `axis`, `value`, `limit`, `measured`, `drive_vpk` (the raw-sample watchdog, which reads the input samples and not the tracked amplitude) |
| `oscillator_range` | `requested_hz`, `fs_hz` |
| `divergence` | `capped_s` (the change cap bound this long without a break) |
| `law_fault` | `reason`, `consecutive` |

A `law_fault` trip's `reason` is `error` (the law reported one, or could not be built),
`panicked` (it panicked or, for a law file, trapped), `over_budget`, `over_memory` (a law file),
`non_finite` (the drive or a value), `negative` (the drive), `undeclared_value` or
`too_many_values`.

### Sine Run Folders

Every Sine run that arms leaves a run folder as Random's does (Run Folders, and
`docs/run-folder.md`, "What a Sine Run Saves"), named `<YYYYMMDD-HHMMSS>_sine_<run id>`: claimed
at arm and never written over, `run.json`, `table.json`, `setup.json` (the configuration as
accepted; what it resolved to: the profile's breakpoints with their quantity and the
acceleration they give, the test's sweep law, rate, direction, count, dwell and step settings,
the level ladder, the alarm and abort bands, the limit channels and the notch, the control
settings and the controller's own configuration; the derived values; feasibility) and
`law/law.json` (the law's identity, parameters and guards) before the first sample, or the arm
is refused `record_unwritable`. Every `sine.log` entry is appended as it is made, with the
station frame and structured `data` for each state, level, alarm, trip, overload, override and
operator action; the inputs and the drive as the device accepted it are streams unless
`record.streams` is false; the results (the profile and its bands on the run's band, each limit
channel's lines, the identification's transfer function and coherence, each input's tracked
level and phase as the run went, and every control-law call) are written as the run goes, off
the control thread; `end.json` holds `end` as `sine.state` reports it. A failure to write the
record is a log entry of kind `record` and the state's `record.problem`; the run goes on.

### Sine Display

Event topics:

- `sine.state` (text): the state object on every change of `state`, `phase`, `mode`,
  `level_db`, `held`, `alarm`, `notch` or `end`.
- `sine.log` (text): new log entries, as Random's, with `law` entries for live parameter
  changes and unusable calls.
- `sine.live` (binary, the **default subscription**, 5 times a second from the start of
  identification until the run has finished, then once more).
- `sine.data` (binary, to one subscribed connection): `sine.live`'s fields but `views`, and
  `plots`, at most `rate_hz` times a second, and once at once on subscribe.

```
{"run_id","source","class","state","phase","test","mode","t_s","elapsed_s","frequency_hz",
 "direction","sweep_rate_oct_per_min","sweeps","held","level_db","commanded_db",
 "target","measured","error_db","drive_vpk","cola_vpk","notch","alarm",
 "channels":[{"index":1,"description":"Control","role":"control","amplitude":0.501,
              "phase_deg":-12.4,"rms":0.355,"unit":"g","overload":false}],
 "alarms":[{"kind":"band","side":"high","error_db":1.7}],
 "law":{"values":{"servo_step_db":0.02,"prediction_step_db":0.01,"total_step_db":0.03,
                   "servo_level_db":-12.5},
        "misses":0,"capped_ticks":0},
 "views":[control, drive, run readings]}
```

- `t_s` is the station time of the tick the frame's numbers are from, since the loop engaged,
  holds included: a sweep rate or a hold is judged on `t_s`, never on when the frame arrived.
  `elapsed_s` is test time (holds excluded).
- `sweep_rate_oct_per_min` is the rate now, signed (negative sweeping down, 0 at a dwell or
  held); a test whose rate is linear (a linear sweep, or a resonance search at a linear rate)
  reports its own rate as `sweep_rate_hz_per_s` instead. A frame carries exactly one of the
  two.
- `level_db` is the level the drive is at now and `commanded_db` the scheduled level; they
  differ while the drive comes onto a new level.
- `target` and `measured` are the profile at the present frequency and level and the composite
  control amplitude, in the control unit, peak; `error_db` is
  `20 log10(measured / target)` (null until both are known). `drive_vpk` is the drive as played,
  `cola_vpk` the reference output's (null without COLA).
- `channels` lists every enabled input in table order: its tracked amplitude at the tone's
  frequency (peak), its phase against the drive, degrees, its broadband rms, its unit, and
  `overload`: for a control row, true once its tracked amplitude has reached 95 % of the top
  of the profile's abort band at the present frequency and level, and then true to the end of
  the run; false for every other row. It is not Random's input-range latch: it says the control
  point came near its abort bound.
- `alarms` lists what holds now: `{"kind":"band","side","error_db"}` and
  `{"kind":"overload","channel"}` (from the latch above, so it stays once raised). An alarm
  warns; it does not stop the run.
- `law` is the law's values on its last call, by the keys its manifest declares, its unusable
  calls in a row and the ticks a cap has bound; null until the law is built.
- `views` are **Control** (`spectrum`, y `Amplitude` in the control unit, log: `Control`
  (`measured`) on the points the run has reached, then `Profile` (`reference`), `Alarm high`,
  `Alarm low` (`tolerance`), `Abort high` and `Abort low` (`abort`) at the present level),
  **Drive** (y `Drive` V log: `Drive` (`drive`), and with COLA `Reference output` (`other`)),
  and **Run** (a `reading`: `Frequency` Hz, `Direction` (a sweep), `Level` dB, `Target` and
  `Measured` in the control unit, `Error` dB, `Drive` V, `Sweeps` (`"<done> of <of>"`, or the
  count alone; a sweep), `Elapsed` s and `Remaining` s (when known); `Measured` and `Error`
  read `pass` inside the alarm band and `warn` outside it, the rest `info`). In a sweep or a
  step test their x is `Frequency` Hz log on the sweep grid (`link` `frequency`); in a dwell
  or a resonance search it is `Time` s linear, the station time `t_s` of each tick over the
  last 60 s, at most 1200 points (`link` `time`).

`sine.subscribe` takes Random's form: `{"rate_hz","plots":[plot...]}` (`rate_hz` 0.5 to 10,
default 5; at most 64 plots, each `id` unique, 1 to 64 characters) or `{"default":true}` (back
to `sine.live`); a key a quantity does not take is `bad_params` naming it. A plot is
`{"id","quantity","channels","points"}`; `channels` applies to `response`, `phase` and `time`
(a list of indexes, or `"all"`, `"control"` or `"limited"`, as Random's), `points` to `time`
only (64 to 8192, default 1024). The result is `{"default","rate_hz","plots","quantities"}`,
every default filled in: a plot keeps `channels` (default `"all"`) only for `response`, `phase`
and `time`, and `points` only for `time`. In `sine.data` each plot is
`{"id","quantity","channels","views"}`, `channels` the indexes it resolved to in table order
for `response`, `phase` and `time` and null for the rest. Plots against frequency are on the run's sweep grid (256 log-spaced
points across the band the station adopted, with its exact ends) in a sweep or a step test,
and hold the latest tick at each point, a measured series carrying only the points the run
has reached; in a dwell or a resonance search they are against time, the station time of the
last 60 s, at most 1200 points.

| `quantity` | Views | What they show |
|---|---|---|
| `control` | one `spectrum` | The live frame's **Control** view |
| `drive` | one `spectrum` | The live frame's **Drive** view |
| `response` | one `spectrum` per resolved channel | Titled `Channel <index>: <description>` (the role when there is no description), y `Amplitude` in the channel's unit, log: `Channel <index>` (`measured`), its tracked amplitude, peak; for a limit channel also `Limit` (`limit`), its bound (the top of its profile's abort band) at every point |
| `phase` | one `spectrum` per resolved channel | Titled as `response`, y `Phase` deg linear: `Channel <index>` (`measured`), its phase against the drive played on the same frame, in (-180, 180], the station's whole drive-to-input delay included |
| `thd` | one `spectrum` | **Distortion**, y `THD` % linear: `THD` (`measured`), the control channel's total harmonic distortion |
| `time` | one `spectrum` per resolved channel | The last batch's acquired samples, as Random's (x `Time` s from the batch's first sample, no `link`) |
| `identification` | **Identification Gain**, **Coherence** (`spectrum`s, x `Frequency` Hz log) | `Gain` (`measured`), `abs(H)` to the composite control point (control unit peak per volt peak) on the identification's grid inside the band (a dwell's: an octave either side), at most 1024 points log-spaced, and `Trust floor` (`tolerance`, `trust_h_floor_frac` of the in-band peak, at the grid's first and last point); `Coherence` (`measured`), `Trusted` (`reference`, the coherence at the frequencies the identification trusts) and `Trust coherence` (`tolerance`, at the same two points) |
| `law` | one `reading` | **Law**: each declared value on the last call by its label and unit, then `Unusable calls in a row` (`pass` at 0, `warn` above) and `Ticks a cap bound` |
| `readings` | one `reading` | The live frame's **Run** reading |

A plot is empty (`views` empty) until there is something to draw: `identification` until the
gate has run, the others until the loop has engaged.

## Mixed Mode

**Status.** This section is the contract the Mixed Mode service and screen are built against;
the service serves every method below. Where a figure the core does not report yet is named
(a trip's measured and limit values, an envelope alarm's worst line), the field is present and
null.

A Mixed Mode test plays a list of **parts** together on one drive and controls them together:
exactly one **floor** (broadband random to a breakpoint profile), up to 12 **tones** (sines of a
stated peak amplitude) and up to 12 **narrowbands** (random of a stated density added over the
floor across a stated width). A tone or a narrowband stands still at a frequency (`fixed`),
sweeps between two (`sweep`), or follows another part's frequency at a ratio (`follow`). The
parts list decides the test: a floor and tones is sine-on-random, a floor and narrowbands is
random-on-random, all three is both.

The service identifies the system with Random's broadband identification and coherence gate,
holds for review, engages the floor alone at the schedule's start level, and once the floor has
equalized ramps the tones and narrowbands in. Every loop it subtracts each tracked tone from the
control signal, estimates the floor on what is left (the residual), leaves out of the floor's
average the lines a dwelling tone or a narrowband covers, calls the floor law on the floor alone,
servoes each narrowband's level in the band's own moving frame, and synthesizes the random part
(floor and narrowbands) ahead; every control tick it closes each tone's loop with the tone law.
One level schedule moves every part by the same dB. The floor's law is a `single`-world law, the
tones' law a `sine`-world law (one choice for every tone, one instance per tone); the narrowbands'
servo is the service's own. The drive is in volts on hardware. A Mixed Mode run is one test like
any other and excludes every other run (`run_active`). **Any connected screen may stop or abort**
(`mixed.stop`, `mixed.abort`); `mixed.configure` (other than a dry run), `arm`, `start`,
`proceed`, `level`, `mode`, `hold`, `resume`, `part`, `adjust`, `reset` and `clear` need control;
everything that only reads or only computes is open to every connection. While the service is
shutting down, `mixed.arm`, `start`, `proceed`, `level`, `mode`, `resume`, `part` and `adjust`
are refused `shutting_down`; `mixed.hold`, `stop` and `abort` are not.

| Method | Params | From | Result |
|---|---|---|---|
| `mixed.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived","views"}` |
| `mixed.config` | `{}` | any | `{"run_id","config","derived","views"}` |
| `mixed.arm` | `{"override_ack"}` (optional) | `configured`, `blocked` | state |
| `mixed.start` | `{}` | `configured`, `armed` | state |
| `mixed.proceed` | `{}` | `review` | state |
| `mixed.level` | `{"delta_db"}` or `{"to_db"}` | `running`, in `manual` | `{"level_db"}` |
| `mixed.mode` | `{"mode":"auto"\|"manual"}` | any configured state | `{"mode"}` |
| `mixed.hold` | `{}` | `running` | state |
| `mixed.resume` | `{}` | `running`, held | state |
| `mixed.part` | `{"id","on"}` | `running`, once the parts have come in (`engaged` or `full_level`) | `{"id","on"}` |
| `mixed.adjust` | `{"key","value"}` | `running`, once the tone law is built (after `mixed.proceed`) | `{"key","value"}` |
| `mixed.stop` | `{}` | any | state |
| `mixed.abort` | `{}` | any | state |
| `mixed.reset` | `{}` | any but live | state |
| `mixed.clear` | `{}` | any but live | state |
| `mixed.state` | `{}` | any | state |
| `mixed.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `mixed.subscribe` | see "Mixed Mode Display" | any | as there |

"state" in the Result column is the `mixed.state` object below. A run is live while it is armed,
running, holding for review or stopping. `mixed.configure` replaces a run that is not live;
`mixed.reset` configures the present run's configuration again on the present table and
station (a new `run_id`); `mixed.clear` forgets it (`unconfigured`). A result that carries views
travels as a binary frame. A tone's amplitude profile is a Sine profile, so a screen reads one
from a file or pasted rows with `sine.profile.import`, which only computes.

### Mixed Mode Configuration

```
{"title":"Rotor on random, 20 to 2000 Hz",
 "parts":[
   {"id":"floor","name":"Floor","kind":"floor","enabled":true,"window":{"on_s":0,"off_s":null},
    "breakpoints":[{"freq_hz":20,"psd_g2_hz":0.01},{"freq_hz":80,"slope_db_oct":3},
                   {"freq_hz":350,"psd_g2_hz":0.04,"alarm_db":[2,2]},
                   {"freq_hz":2000,"slope_db_oct":-3}],
    "alarm_db":[3,3],"abort_db":[6,6]},
   {"id":"t1","name":"Blade pass","kind":"tone","enabled":true,"window":{"on_s":0,"off_s":null},
    "schedule":{"kind":"fixed","freq_hz":120,"end":null},
    "amplitude_g":2,"alarm_db":[1.5,1.5],"abort_db":[3,3]},
   {"id":"t2","name":"Rotor","kind":"tone","enabled":true,"window":{"on_s":0,"off_s":null},
    "schedule":{"kind":"sweep","band_hz":[300,600],"rate":{"law":"log","oct_per_min":1},
                "start_hz":300,"direction":"up","end":{"sweeps":4,"duration_s":null}},
    "profile":{"label":"Rotor","rows":[{"freq_hz":300,"quantity":"accel_g","value":1},
                                       {"freq_hz":600,"quantity":"accel_g","value":1.5}]},
    "alarm_db":[1.5,1.5],"abort_db":[3,3]},
   {"id":"t3","name":"Rotor 2nd","kind":"tone","enabled":true,"window":{"on_s":0,"off_s":null},
    "schedule":{"kind":"follow","of":"t2","ratio":2},
    "amplitude_g":0.5,"alarm_db":[1.5,1.5],"abort_db":[3,3]},
   {"id":"b1","name":"Gear mesh","kind":"narrowband","enabled":true,
    "window":{"on_s":0,"off_s":null},
    "schedule":{"kind":"fixed","freq_hz":250,"end":null},
    "level_g2_hz":0.2,"width_hz":20,"alarm_db":null,"abort_db":null},
   {"id":"b2","name":"Rotor band","kind":"narrowband","enabled":true,
    "window":{"on_s":10,"off_s":250},
    "schedule":{"kind":"sweep","band_hz":[1300,1800],"rate":{"law":"linear","hz_per_s":2},
                "start_hz":1300,"direction":"up","end":null},
    "level_g2_hz":0.1,"width_share":0.05,"alarm_db":[3,3],"abort_db":[6,6]}],
 "exclusion_hz":2,
 "acquisition":{"fs_hz":8192,"n_pts":16384,"n_avg":4,"tick_ms":10},
 "schedule":{"mode":"auto","levels":[{"db":-12,"dwell_s":30},{"db":-6,"dwell_s":30},{"db":0}],
             "full_level_s":300},
 "control":{"composite":"weighted_avg","weights":[{"channel":1,"weight":1}],
            "control_dof":120,"damping":4,"max_lines_out":10,"max_loops_out":5,"max_sigma":5,
            "drive_ceiling_vrms":1,"drive_abort_vrms":null,"coherence_floor":0.5,
            "review_after_sysid":true,
            "pretest":{"averages":4,"level_db":-20,"max_vrms":1,"start_vrms":0.01,"shaped":false},
            "soft_start_db":9,"soft_start_relax_db":6,"level_ramp_s":3,"shutdown_ramp_s":3,
            "part_ramp_s":3,"floor_settle_s":30,
            "trust_coherence":0.5,"trust_h_floor_frac":0.05,
            "tracking":{"kind":"fixed","hz":0.5},
            "law":{"floor":{"id":"hinv","params":{}},
                   "tones":{"id":"servo_blend","params":{"blend_w":0.5,"compression":"slow",
                                                        "kp":0.3}}},
            "law_guards":{"floor":{"law_budget":0.25,"law_misses":3,"max_change_db":6},
                          "tones":{"law_budget":0.25,"law_misses":3,"max_step_db":1,
                                   "max_rise_db_per_s":240,"divergence_s":1,
                                   "fault_debounce_ticks":50}}},
 "limits":{"drive_vpk_ceiling":4,"drive_vpk_abort":6,"kinematic_monitor":true,
           "channels":[{"channel":3,"kind":"response",
                        "rows":[{"freq_hz":20,"ceiling":0.5},{"freq_hz":2000,"ceiling":0.5}]}]},
 "feasibility":{"shaker":{"name":"Bench shaker",...},
                "moving_mass_lb":{"armature":45,"slip_table":0,"drive_adapter":3,"fixturing":10,
                                  "test_article":12},
                "crest_factor":3},
 "operator":"A. Operator","article":"Rotor housing, serial 4","notes":"first article",
 "specification":{"document":"Test procedure 12","clause":"4.3"},"record":{"streams":true},
 "metadata":[{"key":"run_number","label":"Run Number","value":"7"}]}
```

**Parts.** `parts` lists every part in the order the run reports them; that order is the order of
every per-part list in the derived values, the state and the frames. Every part carries:

| Field | Default | Meaning |
|---|---|---|
| `id` | (required) | The part's name for the wire: 1 to 32 characters of letters, digits, `_` and `-`, unique in the list (`duplicate_part_id`). Every per-part row, trip, alarm, log line and plot names its part by `id` |
| `name` | the kind and its count (`Floor`, `Tone 2`, `Narrowband 1`) | The part's name for a person; a run-end message names the part by it ("Tone 3 abort") |
| `kind` | (required) | `floor`, `tone` or `narrowband`; any other kind is refused `unknown_part_kind` at `parts[<i>].kind` |
| `enabled` | true | Whether the part plays from its window's start. A part that is not enabled stays off until `mixed.part` turns it on. The floor is always enabled (`false` is `out_of_range`) |
| `window` | `{"on_s":0,"off_s":null}` | When the part plays, in **parts time** (below): it ramps in at `on_s` (0 or more) and out at `off_s` (above `on_s`; null: to the end of the run). The floor's window is `{"on_s":0,"off_s":null}` and nothing else (`out_of_range`) |

The fields of each kind sit beside these, in the part itself:

| Kind | Field | Default | Meaning |
|---|---|---|---|
| `floor` | `breakpoints` | (required) | Random's profile rows (see "Random Test Configuration"), in g²/Hz: each a PSD anchor or a slope, the first an anchor, frequencies rising strictly, with optional per-row `alarm_db` and `abort_db`. The floor's band, from its first row to its last, is the test's band |
| `floor` | `alarm_db`, `abort_db` | `[3,3]`, `[6,6]` | Random's `[below, above]` bands in positive dB, judged on the floor's clean lines |
| `tone` | `schedule` | (required) | How its frequency moves (below) |
| `tone` | `amplitude_g` or `profile` | (one required) | Its peak amplitude in g: `amplitude_g`, a constant above 0, or `profile`, a Sine profile (see "Sine Configuration") read at the tone's frequency, which must cover the tone's whole motion (`profile_not_covering`). Both, or neither, is `bad_params` |
| `tone` | `alarm_db`, `abort_db` | `[1.5,1.5]`, `[3,3]` | Sine's `[below, above]` bands in positive dB around the amplitude. A `profile`'s own bands, and its rows', take their place where it gives them |
| `narrowband` | `schedule` | (required) | How its center moves (below) |
| `narrowband` | `level_g2_hz` | (required) | The density **added** over the floor between the band's edges, g²/Hz, above 0: the band's reference is the floor's reference plus this level, line by line |
| `narrowband` | `width_hz` or `width_share` | (one required) | The band's full width: in Hz (above 0), or as a share of its center (above 0, at most 1), so a band given by share widens as it climbs. Both, or neither, is `bad_params` |
| `narrowband` | `alarm_db`, `abort_db` | null: the floor's profile bands | `[below, above]` in positive dB around the band's reference, each widened to the band's own chi-square scatter at 90 % confidence at the degrees of freedom it holds |

**Schedules.** `schedule.kind` is one of:

| `kind` | Fields | Meaning |
|---|---|---|
| `fixed` | `freq_hz`; `end` (default null) | The part stands at `freq_hz` (above 0). `end` is Sine's dwell end, `{"seconds"}` or `{"cycles"}` (a whole number), or null to stand for the whole window. When it is reached the part ramps out |
| `sweep` | `band_hz`; `rate`; `start_hz` (default `band_hz[0]`); `direction` (default `up`); `end` (default null) | Sine's sweep: `band_hz` `[lo, hi]` (`0 < lo < hi`), `rate` `{"law":"log","oct_per_min"}` or `{"law":"linear","hz_per_s"}` (above 0), `start_hz` inside the band, `direction` the first leg. The part bounces between the band's ends. `end` is Sine's `{"sweeps","duration_s"}`: the one-way sweeps, or seconds of parts time, or whichever comes first with both, counted from the part's switch-on; null sweeps until the window closes. When it is reached the part ramps out |
| `follow` | `of`; `ratio` | The part's frequency is part `of`'s frequency now times `ratio` (above 0), including its direction, its holds and its jumps at a sweep's ends. `of` names another tone or narrowband (`follow_unknown_part`, `follow_self`); chains are followed in order and a loop is refused (`follow_loop`). A follower has no `end`: it plays while its own window is open, whether or not its leader is on |
| `segments` | | Reserved: one part that sweeps, dwells and sweeps on. Refused `schedule_unsupported` at `parts[<i>].schedule.kind` |

**Parts time.** The parts' clocks are one clock, `parts_s`: 0 when the floor has equalized at the
schedule's start level and the tones and narrowbands first may come in, counted in station time,
holds excluded. A window's `on_s` and `off_s`, a `fixed` part's `end.seconds` and a `sweep`'s
`end.duration_s` are on it. A part's own schedule clock starts when the part switches on (its
`on_s`, or `mixed.part`) and stops while it is off; a `follow` part reads its leader's frequency
at the same moment whether or not its leader is playing (a leader that has not yet switched on
stands at its start frequency).

**Motion checks** (all refused by name in `config_invalid`, at `parts[<i>]`): every tone's
frequency, and every narrowband's center and both its edges, must stay inside the floor's band
at every point of its motion, through every `follow` ratio and at both ends of every sweep
(`part_out_of_band`); two tones must stay at least two tracking widths (`2 x control.tracking.hz`)
apart through the whole of both motions (`parts_too_close`), except a tone that follows another
at a ratio whose motion keeps them apart by that much. At most 12 tones (`too_many_tones`) and
12 narrowbands (`too_many_bands`); exactly one floor (`no_floor` when there is none,
`too_many_floors` when there are more). A tone may stand inside a narrowband.

**The rest of the configuration.** Where a field is Random's or Sine's, its name, shape, default
and meaning are theirs, and the field applies as stated here:

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `exclusion_hz` | 2 | The width, Hz each side, around a tone that stands still (a `fixed` tone, or one that follows a `fixed` part) whose lines are left out of the floor's average; above 0, at most 50. A narrowband's lines, its edges plus two lines each side, are left out wherever the band is. A tone that sweeps leaves no line out. Lines left out are given the floor law's error interpolated in dB from the nearest clean lines either side |
| `acquisition.fs_hz`, `n_pts`, `n_avg` | (required) | Random's grid: the rate requested of the station, the FFT length (a power of two) and the frames per control loop (Hann, half overlap). The lines are `fs_hz / n_pts` apart; with tones they must be no wider than half of `exclusion_hz`, so a tone's exclusion covers at least two lines (`out_of_range` at `exclusion_hz`) |
| `acquisition.tick_ms` | 10 | Sine's control tick, ms, converted to whole frames at the rate the station adopts: 2 to 1000 for a built-in tone law, 8 to 1000 for a law file (`tick_out_of_range`) |
| `schedule` | (required) | Random's ladder, `{"mode","levels":[{"db","dwell_s"}],"full_level_s"}`, moving every part by the same dB. In `auto` the run engages at the first level, holds each for its `dwell_s` of parts time and full level for `full_level_s`, then ends `completed`; in `manual` it moves only on `mixed.level` |
| `control.composite`, `weights` | `weighted_avg`, 1 each | Random's, for the floor, the narrowbands and each tone's composite tracked amplitude |
| `control.control_dof`, `damping`, `max_lines_out`, `max_loops_out` | 120, 4, 10, 5 | Random's, for the floor, on its clean lines |
| `control.max_sigma` | 5 | Random's clip, on the random part (floor and narrowbands) alone, before the tones are added |
| `control.drive_ceiling_vrms`, `drive_abort_vrms` | 10, null | Random's, on the random part's synthesis (the floor and narrowbands, not the tones) |
| `control.coherence_floor`, `review_after_sysid`, `pretest` | 0.5, true, Random's | Random's identification and its gate (there is no sine identification): its gain, coherence and lag on the line grid give the floor law its plant and each tone its feed-forward and its law's identification at the tone's frequency |
| `control.soft_start_db`, `soft_start_relax_db` | 9, 6 | Random's, for the floor alone |
| `control.level_ramp_s`, `shutdown_ramp_s` | 3, 3 | Random's: a level change ramps every part together over `level_ramp_s`; the controlled stop ramps every part's amplitude together linearly to zero over `shutdown_ramp_s` (6 dB down halfway) |
| `control.part_ramp_s` | 3 | The ramp of one part coming in or going out (its window, its `end`, `mixed.part`), above 0, at most 60 s. Tones come in from their feed-forward amplitude |
| `control.floor_settle_s` | 30 | The most station time the floor alone is given at the start level to equalize (above 0, at most 600). It has equalized once its estimate holds `control_dof` degrees of freedom with its rms within 1 dB of its reference; if the grid needs longer than `floor_settle_s` to reach `control_dof` (`derived.floor_equalize_s`), the parts wait for that; the log states both |
| `control.trust_coherence`, `trust_h_floor_frac` | 0.5, 0.05 | Sine's: where the identification is trusted for a tone's feed-forward and its law (a tone at a frequency it does not trust has no prediction there) |
| `control.tracking` | `{"kind":"fixed","hz":0.5}` | Sine's form; Mixed Mode tracks every tone at one fixed width, Hz, above 0, at most 10 (`{"kind":"proportional"}` is `out_of_range`). A tone's reading is used once its tracker has settled (1.3 over the width, s) |
| `control.law.floor` | `{"id":"hinv"}` | The floor's law, by catalog `id` in world `single` (see "Control Laws"), with `params` by key; defaults fill the rest |
| `control.law.tones` | `{"id":"servo_blend"}` | The tones' law, by catalog `id` in world `sine`, with `params` by key: one choice, one instance per tone |
| `control.law_guards.floor` | `{"law_budget":0.25,"law_misses":3,"max_change_db":6}` | The floor law's guards: the fraction of one loop's acquisition time a call may take (above 0, at most 1), the unusable calls in a row that stop the run (1 to 100), and the most any drive line may change in one call, dB (never tighter than the built-in's own worst step plus 1 dB, as Random's) |
| `control.law_guards.tones` | Sine's | Sine's `law_budget`, `law_misses`, `max_step_db`, `max_rise_db_per_s`, `divergence_s` and `fault_debounce_ticks`, with Sine's defaults and ranges, for each tone |
| `limits.drive_vpk_ceiling` | (required) | Sine's: volts peak, above 0, at most 10. The summed drive (random part plus tones) is held at or under it, judged on every frame; how the cut is shared between the parts is the service's, and `limiting` says when it binds |
| `limits.drive_vpk_abort` | (required) | Sine's: volts peak, at least the ceiling (`abort_below_ceiling`) and at most 10. A summed drive sample at or over it aborts the run (`drive_vpk`) |
| `limits.kinematic_monitor` | true | Sine's: with `feasibility`, abort when the response's velocity or displacement, read on raw samples (tones in), passes the shaker's rating (`kinematic`). Off, the station's own raw-sample watchdog still trips on each channel's own velocity limit: the switch does not turn raw-sample protection off |
| `limits.channels` | none | Random's limit rows, `{"channel","kind","rows":[{"freq_hz","ceiling"}]}`, on any enabled input, a control channel too, as Random's, ceilings in the channel's unit squared per Hz. The service lowers the random part where a channel would pass its ceiling; a limit never aborts, and it does not lower a tone |
| `feasibility` | null | Random's shaker record, moving mass and `crest_factor` (default 3). Checked at configure and arm against the predicted peaks (below) |
| `operator`, `article`, `notes`, `specification`, `record` | empty, null, `{"streams":true}` | Random's, for the run's record (see Mixed Mode Run Folders): the operator's words, the document and clause the test is run to, and whether the record keeps the inputs and the drive as streams. None of them changes how the run drives |
| `metadata` | the default fields | Random's |

Mixed Mode takes no `capture` (and so no `periodic_save_s`): every run is recorded as it goes
(see Mixed Mode Run Folders), and a configuration that gives one is refused `bad_params` at
`config.capture`.

The control channels are the table's enabled `control` rows; the drive is its first enabled
`drive` row. A configuration whose shape is wrong is refused `bad_params` naming the field in
`detail.param` as the request's own path (`config.parts[2].amplitude_g`); its content errors are
refused `config_invalid` (code `bad_request`, as Random's and Sine's) with the items, located by
`field`, the path inside the configuration (`parts[3].schedule.of`,
`control.law_guards.tones.max_step_db`), a floor breakpoint row or a tone profile row by its
index in that path (`parts[0].breakpoints[2]`, `parts[2].profile.rows[1]`).
`dry_run: true` checks without replacing anything and needs no control.

**Result.** `state` is the `mixed.state` object (`configured`). `derived` is what the service
computes at configure, at full level (0 dB):

```
{"band_hz":[20,2000],
 "grid":{"fs_hz":8192,"n_pts":16384,"df_hz":0.5,"lines":3961,"loop_s":8,
         "tick_frames":82,"tick_s":0.0100098,"actual":false},
 "control_channels":[1],
 "parts":[{"id":"floor","kind":"floor","name":"Floor","freq_hz":null,"width_hz":null,
           "ms_g2":36.75,"peak_g":null},
          {"id":"t1","kind":"tone","name":"Blade pass","freq_hz":[120,120],"width_hz":null,
           "ms_g2":2,"peak_g":2},
          {"id":"t2","kind":"tone","name":"Rotor","freq_hz":[300,600],"width_hz":null,
           "ms_g2":1.125,"peak_g":1.5},
          {"id":"t3","kind":"tone","name":"Rotor 2nd","freq_hz":[600,1200],"width_hz":null,
           "ms_g2":0.125,"peak_g":0.5},
          {"id":"b1","kind":"narrowband","name":"Gear mesh","freq_hz":[250,250],
           "width_hz":[20,20],"ms_g2":4,"peak_g":null},
          {"id":"b2","kind":"narrowband","name":"Rotor band","freq_hz":[1300,1800],
           "width_hz":[65,90],"ms_g2":9,"peak_g":null}],
 "reference":{"floor_ms_g2":36.75,"tones_ms_g2":3.25,"bands_ms_g2":13,"total_ms_g2":53,
              "total_rms_g":7.28,"random_rms_g":7.053},
 "kinematics":{"peak_accel_g":25.16,"peak_velocity_ips":7.664,"peak_disp_pp_in":0.04887,
               "verdict":"pass","message":"..."},
 "exclusion":[{"part":"t1","lo_hz":118,"hi_hz":122,"lines":9},
              {"part":"b1","lo_hz":239,"hi_hz":261,"lines":45}],
 "closest_tones":{"parts":["t1","t2"],"spacing_hz":180,"min_hz":1},
 "tracking":[{"part":"t1","verdict":"ok","ratio":0,"at_hz":null,"message":null},
             {"part":"t2","verdict":"ok","ratio":0.002,"at_hz":null,"message":null},
             {"part":"t3","verdict":"ok","ratio":0.004,"at_hz":null,"message":null}],
 "test_s":360,"floor_equalize_s":70,
 "tone_judging":[{"part":"t1","judge_s":0.5,"sigma_db":0.12,"alarm_db":[1.5,1.5],"abort_db":[3,3],
                  "widened":false}, ...],
 "identification_s":32,
 "call_budget_s":{"floor":2,"tones":0.0025024},
 "law":{"floor":{"id":"hinv","name":"...","version":"...","sha256":null,"live":[],
                 "hardware":true},
        "tones":{"id":"servo_blend","name":"Servo blend","version":"0.1.0","sha256":null,
                 "live":["blend_w"],"hardware":true}},
 "feasibility":{"verdict":"pass","message":"..."}}
```

- `band_hz` is the floor's band; `grid` Random's grid with Sine's tick in whole frames and in
  seconds (7 decimals) at the requested rate, `actual` true once the run has armed and every
  number is the station's.
- `parts`, in the parts' order, one row each: `freq_hz` the lowest and highest frequency (a
  tone) or center (a narrowband) of the part's whole motion, through every ratio (null for the
  floor); `width_hz` a narrowband's narrowest and widest width (null otherwise); `ms_g2` the
  part's reference mean square, g²: the floor's integral, `A²/2` for a tone at the largest
  amplitude of its motion, level times width for a narrowband at its widest; `peak_g` a tone's
  largest amplitude.
- `reference` sums them: `total_ms_g2` is floor plus tones plus narrowbands (the composite's
  reference, which is shown and not judged), `total_rms_g` its root, `random_rms_g` the random
  part's (floor plus narrowbands).
- `kinematics` is the core's prediction of the peak of the sum (null when it cannot predict),
  each to 4 significant digits: acceleration as the
  tones' amplitudes summed plus `crest_factor` times the random part's rms; velocity and
  displacement as each tone's at the frequency of its motion where it is largest plus
  `crest_factor` times the random part's (the narrowbands at their lowest centers); displacement
  peak to peak. `verdict` and `message` are Sine's, against the shaker's ratings with
  `feasibility`, `not_evaluated` without it.
- `exclusion` lists the spans left out of the floor's average for the whole run: one per tone
  that stands still and one per narrowband that stands still, with `lines`, the grid lines in
  it. A moving narrowband leaves out the lines under it as it passes; they are counted live.
- `closest_tones` is the closest pair of tones over their whole motion, their spacing then and
  the least allowed (two tracking widths); null with fewer than two tones.
- `tracking` is Sine's verdict for each tone, in the parts' order, on the fixed tracking width:
  `ok`, `advisory` (a warning `tracking_lag`) or `refused` (an error `tracking_lag`).
- `test_s` is the ladder's length in parts time (the dwells and `full_level_s`; null in
  `manual`); `floor_equalize_s` the core's prediction of the station time the floor needs to
  reach `control_dof` degrees of freedom on this grid: the parts come in after the larger of it
  and `floor_settle_s`; `identification_s` the identification's measuring time (`pretest.averages`
  loops); `call_budget_s` one floor law call's budget (a fraction of the loop's acquisition
  time) and one tone law call's (a fraction of the tick), 7 decimals.
- `tone_judging` is the core's prediction of how each tone will be judged, one
  `{"part","judge_s","sigma_db","alarm_db","abort_db","widened"}` per tone in the parts' order,
  at the point of its motion that needs the longest averaging: the averaging, seconds; the judged
  figure's scatter, dB at one sigma; the bands judged against, `[below, above]` dB; whether they
  are widened. Empty when the prediction cannot be made.
- `law` is each role's law, its live parameter keys (a `single` law has none), and `hardware`:
  whether it may drive hardware in this role (a built-in may; an imported law needs a passed
  check in its own world and a passed mixed check in this role, see "Control Laws").

`views` are **Reference** (`spectrum`, x `Frequency` Hz log, y `PSD` g²/Hz log, `link`
`frequency`): `Reference` (`reference`), `Alarm high` and `Alarm low` (`tolerance`), `Abort high`
and `Abort low` (`abort`), the floor plus each narrowband at its starting center, at 0 dB, on
the band's lines of the requested grid, each `shape: "steps"` with its `edges`, as Random's; and
**Tones** (`spectrum`, x `Frequency` Hz log, y `Amplitude` g log): one series per tone
(`reference`, `shape: "line"`), named by the part's name, its amplitude across its motion (a
`fixed` tone a single point).

`mixed.config` returns what the present run was configured with: its `run_id`, the `config` as
accepted, its `derived` values and the same `views`; all null (`views` empty) when nothing is
configured. `derived.grid` stays the requested grid; the run state's `grid` is the run's.

Item reasons (Mixed Mode): errors as Random's for the floor's breakpoints, the acquisition, the
ladder, the control channels and the limit rows, as Sine's for a tone's profile and the tone
guards, and `no_floor`, `too_many_floors`, `unknown_part_kind`, `duplicate_part_id`,
`part_out_of_band`, `parts_too_close`, `too_many_tones`, `too_many_bands`, `follow_unknown_part`,
`follow_self`, `follow_loop`, `schedule_unsupported`, `profile_not_covering`,
`tick_out_of_range`, `abort_below_ceiling`, `no_control_channel`, `weight_unknown_channel`,
`no_drive_channel`, `limit_unknown_channel`, `tracking_lag`
(refused), `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid`,
`out_of_range`; warnings `alarm_wider_than_abort`, `join_discontinuity` (a tone profile),
`tracking_lag` (advisory).

### Running a Mixed Mode Test

- **`mixed.arm`** is `random.arm` for Mixed Mode: it checks feasibility (when given), opens the
  station, adopts its rate, builds the grid and the tick at that rate, checks the parts' motion
  and the tracking against them again, and passes the arm gate. Nothing is driven. Refusals as
  `random.arm`'s, with `law_not_checked` on a hardware station for an imported law without the
  checks its role needs (`detail.role` `floor` or `tones`).
- **`mixed.start`** from `configured` arms first. It runs Random's identification
  (`phase` `sysid`) and its coherence gate (`coherence_gate`); under `coherence_floor` the run
  ends `held`, an identification that cannot reach its level ends `sysid_level_unreachable`;
  otherwise it holds for review (`review`) or, without one, goes on as `mixed.proceed` does. It
  answers at once.
- **`mixed.proceed`** builds both laws from the identification (a law that cannot be built ends
  the run `law_fault` naming its role before anything plays) and engages the floor alone with
  Random's soft start at the ladder's first level (`phase` `floor_only`). Once the floor has
  equalized (or at `floor_settle_s`), parts time starts, `phase` turns `engaged`, and each tone
  and narrowband whose window is open and that is enabled ramps in over `part_ramp_s`.
- **Each control tick** the service advances every part's clock, steps each tone's oscillator
  (phase-continuous for the whole run), tracks each tone on every control row against its own
  phase, composites the tracked amplitudes, calls the tone law once per tone that is on and
  settled, and caps each tone's drive under Sine's guards. **Each loop** it estimates the
  residual, calls the floor law on the clean lines, servoes each narrowband, synthesizes the
  random part ahead, and judges every part. A late, failed, panicked or refused call keeps that
  role's drive in force; `law_misses` in a row of either role end the run with the controlled
  stop (`law_fault`, the trip naming the role and, for a tone, the tone).
- **`mixed.level`** and **`mixed.mode`** are `random.level` and `random.mode` for Mixed Mode,
  with the same refusals (`manual_mode_required`, `not_a_rung`): every part moves by the same dB
  over `level_ramp_s`. A level change re-opens each tone's settle window, as Sine's does.
- **`mixed.hold`** freezes the test's progress: parts time, every part's schedule clock and
  window, and the level clock stop, and every part stays at its frequency and level, still under
  control (`held` true). **`mixed.resume`** starts them again. Neither is a run state, and
  neither releases a review or a held gate.
- **`mixed.part`** switches the part `id` on (`on: true`) or off (`on: false`) live, as a ramp
  over `part_ramp_s`, and answers once the ramp has begun. A part switched on starts or resumes
  its own schedule clock; one switched off holds it. Switching on a part whose window has closed
  or whose `end` has been reached, or switching off the floor, is refused `part_refused` with
  `detail.id` and `detail.why` (`unknown_part`, `floor`, `window_closed`, `schedule_ended`);
  asking for the state a part is already in answers with it, unchanged.
- **`mixed.adjust`** is `sine.adjust` for the tone law: it changes one live parameter of every
  tone's law instance, from the next tick, and is logged (`law`). Refused `adjust_refused` as
  Sine's. The floor law has no live parameters.
- The run verbs (`hold`, `resume`, `level`, `part`, `adjust`) are taken between the service's
  steps of a quarter second of station time, and answer once the change has been taken.
- **`mixed.stop`** is the controlled stop: every part's amplitude ramps down together, linearly
  from the level in force to zero over `shutdown_ramp_s` (6 dB down halfway; state `stopping`), and the run ends
  `operator_stop` once the drive is at zero. From `armed` or `review` it ends at once. It answers
  at once.
- **`mixed.abort`** ends the run within one batch of ticks without the ramp; the station takes
  the drive down with its own stop. The outcome is `operator_abort`.
- **When nobody holds control** the run goes on; after `timeout_s` it ends `safe_stop` with the
  controlled stop, as Random's. While the service shuts down it ends the same way.

**Time is the station's,** as Sine's: `t_s`, `elapsed_s`, `parts_s`, `at_level_s`, a part's
window and schedule clocks, and `end.at_s` count the station's own samples at the rate it
adopted; only the safe stop's countdown (`orphan_s`) is the service's clock.

### Mixed Mode Run State

```
{"run_id":"mixed-0002","state":"running","phase":"full_level",
 "title":"Rotor on random, 20 to 2000 Hz","source":"SIM",
 "station":{"kind":"sim","class":"sim","devices":[]},
 "grid":{"fs_hz":8192,"n_pts":8192,"df_hz":1,"lines":1981,"loop_s":4,
         "tick_frames":82,"tick_s":0.0100098,"actual":true},"clock":"shared",
 "mode":"auto","levels_db":[-12,-6,0],"level_db":0,"full_level_s":300,
 "t_s":131.4,"elapsed_s":131.4,"parts_s":96.2,"at_level_s":36.2,"remaining_at_level_s":263.8,
 "held":false,"alarm":false,"limiting":false,
 "parts":[{"id":"floor","kind":"floor","name":"Floor","enabled":true,"on":true,"scale":1,
           "freq_hz":null,"direction":null,"follows":null,"sweeps":null,"rate_oct_per_min":null,
           "rate_hz_per_s":null,"width_hz":null,
           "unit":"g","target":6.06,"measured":6.02,"error_db":-0.06,"judge_s":null,"judged_error_db":null,
           "judged_alarm_db":null,"judged_abort_db":null,"tolerance_widened":null,"alarm":false,
           "drive_vpk":null,"drive_vrms":0.58,"gain_db":null,"settled":true,"limiting":false,
           "dof":120,"lines":{"in_band":1981,"clean":1929,"alarm":2,"abort":0,"rms_judged":1929},
           "law":{"misses":0,"capped":0}},
          {"id":"t2","kind":"tone","name":"Rotor","enabled":true,"on":true,"scale":1,
           "freq_hz":412.6,"direction":"up","follows":null,"sweeps":{"done":1,"of":4},
           "rate_oct_per_min":1,"rate_hz_per_s":null,"width_hz":null,
           "unit":"g","target":1.19,"measured":1.21,"error_db":0.15,"judge_s":0.5,
           "judged_error_db":0.13,"judged_alarm_db":[1.5,1.5],"judged_abort_db":[3,3],
           "tolerance_widened":false,"alarm":false,
           "drive_vpk":0.31,"drive_vrms":null,"gain_db":null,"settled":true,"limiting":false,
           "dof":null,"lines":null,"law":{"misses":0,"capped":0}},
          {"id":"b2","kind":"narrowband","name":"Rotor band","enabled":true,"on":true,
           "scale":1,"freq_hz":1472.4,"direction":"up","follows":null,"sweeps":{"done":0,"of":null},
           "rate_oct_per_min":null,"rate_hz_per_s":2,"width_hz":73.6,
           "unit":"g²/Hz","target":0.1046,"measured":0.0991,"error_db":-0.23,"judge_s":null,"judged_error_db":null,
           "judged_alarm_db":null,"judged_abort_db":null,"tolerance_widened":null,"alarm":false,
           "drive_vpk":null,"drive_vrms":null,"gain_db":0.4,"settled":true,"limiting":false,
           "dof":184,"lines":null,"law":null},
          ...],
 "total":{"rms_g":7.21,"reference_rms_g":7.277,"drive_vpk":2.91,"drive_vrms":0.66},
 "identification":{"level_db":-20,"band_coherence":0.98,"gate_coherence":0.5,"lag_frames":0,
                   "trusted_frac":0.97,"passed":true},
 "law":{"floor":{"id":"hinv","name":"...","version":"...","sha256":null,"misses":0,
                 "capped":0,"live":{}},
        "tones":{"id":"servo_blend","name":"Servo blend","version":"0.1.0","sha256":null,
                 "misses":0,"capped":0,"live":{"blend_w":0.5}}},
 "feasibility":{"verdict":"pass","message":"..."},"end":null,
 "record":{"folder":"/home/operator/Hertzforge_Data/20261101-142233_mixed_mixed-0002","streams":"on","problem":null}}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `blocked` | `feasibility_blocked` | Feasibility exceeded; arming needs an acknowledgement |
| `armed` | `armed` | Station open and gated; nothing driven yet |
| `running` | `sysid`, `coherence_gate`, `floor_only`, `engaged`, `full_level` | Identification, its gate, the floor alone at the first level, the ladder with every part, full level |
| `review` | `awaiting_review` | The gate passed; nothing drives until `mixed.proceed` |
| `stopping` | `ramping_down` | A controlled stop or a safe stop is ramping every part down |
| `finished` | `stopped`, `aborted`, `coherence_hold` | Ended; `end` says how |

- `mode`, `levels_db`, `level_db` (the commanded level), `full_level_s`, `at_level_s` and
  `remaining_at_level_s` are Random's; `phase` turns `full_level` once the drive is at full level.
- `t_s` is station time since the floor engaged, holds included; `elapsed_s` the same with holds
  excluded; `parts_s` parts time (null before the parts come in).
- `held` is true while `mixed.hold` holds the progress. `alarm` is true while any part is
  outside its alarm band. `limiting` is true while the drive ceiling, a limit channel or a cap
  holds any part down, or the drive ceiling's last-resort clamp cut the summed drive in the last
  chunk.
- `parts` has one row per part, every part of the configuration in its order, whether on or not
  (a screen finds a part by `id`, never by position alone). Each row:
  - `id`, `kind`, `name`, `enabled` (as configured, or as `mixed.part` last set it), `on` (the
    part is playing now, ramps included: false while it waits for the floor or its window and
    once it has ramped out) and `scale` (its ramp now, 0 to 1).
  - `freq_hz` the tone's frequency or the narrowband's center now (null for the floor);
    `direction` `up` or `down` while it moves, a follower's its leader's (null when it stands
    still); `follows` `{"of","ratio"}`, the part it follows and the ratio, for a part whose
    schedule is `follow` (null otherwise); `sweeps` the one-way sweeps done and, with a count,
    `of` (null unless the part sweeps on its own schedule); `rate_oct_per_min` or
    `rate_hz_per_s` the part's own rate now, signed (negative going down; 0 before the parts
    come in, at a hold, on a stop and once its sweep ends), in the law its schedule (or its
    leader's) states, the other null, both null for a part that stands still. A follower of a
    log sweep moves at its leader's octaves per minute; of a linear sweep, at its leader's Hz
    per second times the ratio. `width_hz` a narrowband's width now (null otherwise).
  - `unit`, `target`, `measured` and `error_db` (`20 log10(measured / target)` for g,
    `10 log10` for g²/Hz; null until both are known): the floor in `g` rms over its clean
    lines; a tone in `g` peak, its composite tracked amplitude against its amplitude now; a
    narrowband in `g²/Hz`, the mean density over the inner half of its width in its own moving
    frame against its reference over the same lines. `alarm` is true while the part is outside
    its alarm band.
  - A tone's alarm and abort judge its reading averaged to its own scatter: a tone well over the
    floor is judged on each reading, a small one on its reading averaged over up to 8 s, and one
    too close to the floor for that against tolerances widened to its scatter at three sigma.
    `judge_s` that averaging, seconds; `judged_error_db` the error judged, dB (`error_db` stays
    the single reading; null until there is one); `judged_alarm_db` and `judged_abort_db` the
    bands judged against, `[below, above]` in positive dB; `tolerance_widened` true when they are
    widened. `judge_s` and the bands are null until the tone's judging is set; all five are null
    for the floor and a narrowband (a band is judged on its own estimate).
  - `drive_vpk` a tone's commanded amplitude in the drive, volts peak, and `drive_vrms` the
    floor's share of the random part, volts rms (the other null); `gain_db` a
    narrowband's servo gain (null otherwise). A narrowband's `drive_vrms` is null: the core does
    not report its share of the random part apart from the floor's.
  - `settled`: a tone's tracker has settled and its reading is used; a narrowband's estimate
    holds at least 16 degrees of freedom; the floor has equalized. `limiting` is true while a
    cap, a limit channel or the drive ceiling holds the part down.
  - `dof` the degrees of freedom of the floor's control estimate (the typical clean line's, the
    median over the lines read in the last loop) or the narrowband's level estimate (null for a
    tone); `lines` (the floor only) `{"in_band","clean","alarm","abort","rms_judged"}`: the
    band's lines, the clean ones read in the last loop, how many of those lie outside the alarm
    and the abort bands, and `rms_judged` the clean lines the floor's rms abort was judged on in
    the last loop, each holding 16 degrees of freedom (0 while that abort is not armed: it arms
    once three quarters of the band's lines qualify).
  - `law` the part's law instance's unusable calls in a row now and the ticks (a tone) or loops
    (the floor) a cap has bound; null for a narrowband.
- `total` is the composite: its rms over the band (tones in), the reference's at the level in
  force (the service scales the full-level reference by the level, as the Control view's
  reference is drawn; the full level's before anything drives), and the summed drive's peak
  (volts) and rms (volts) over the last loop. The composite is shown, not judged.
- `identification` is Sine's identification facts for Random's identification: its level, the
  band coherence against the gate (`gate_coherence` is `control.coherence_floor`), the lag it
  measured, and the fraction of its grid inside the band it trusts; null until the gate has run.
- `law` is each role's law, its unusable calls in a row now (the worst instance's, for the
  tones), the ticks or loops a cap has bound, and the live parameters' values in force.
- `end` is null until the run finishes, then `{"outcome","message","at_s","level_db","ramp_s",
  "trip"}` as Sine's (`at_s` station time from the floor's engage).

| `outcome` | How the run ended |
|---|---|
| `completed` | The ladder finished; every part ramped down with the controlled stop |
| `operator_stop`, `operator_abort`, `safe_stop` | As Random's |
| `aborted` | A protection tripped; `trip` says which and on which part |
| `held` | The coherence gate held, as Random's |
| `sysid_level_unreachable` | As Random's |
| `law_fault` | A role's law gave no usable drive `law_misses` times in a row, or could not be built; that role's drive in force was held through the misses, then the run took its controlled stop. `trip` says which role and why |
| `fault` | The loop or the station failed; `message` is the controller's words |

`trip` is null unless `outcome` is `aborted` or `law_fault`; it has `kind`, `part` (the `id` of
the part it tripped on, or null when it is the summed drive's or a channel's), `at_level_db`,
`at_s` (station time), `at_loop`, `at_tick`, `at_hz` (the part's frequency then; null for the
floor and for a trip of no part), and its kind's facts. `end.message` names the part by its name
("Tone 3 abort: 3.4 dB over its target at 812.0 Hz, at 0.0 dB."):

| `kind` | Part | Facts |
|---|---|---|
| `envelope` | the floor | Random's: `abort_lines`, `worst_db` |
| `abort_band` | a tone | Sine's: `side` (`high` or `low`), `measured_g`, `limit_g`, and `error_db` (the tone's error then); `error_db` is the judged error; `measured_g` the tone's target then times it, `limit_g` the target times its judged abort band on that side (widened when its tolerances are) |
| `band_abort` | a narrowband | `side`, `measured_g2_hz`, `limit_g2_hz`, `dof`, and `error_db`; `measured_g2_hz` and `limit_g2_hz` from the band's reference then, its error and its abort band, as a tone's; `dof` its estimate's degrees of freedom then |
| `servo_fault` | a tone | Sine's: `consecutive_ticks` |
| `divergence` | the floor or a tone | the floor: Random's `consecutive_loops`, `divergent_bins`; a tone: Sine's `capped_s` |
| `law_fault` | the floor, or the tone whose instance failed | `role` (`floor` or `tones`), `reason`, `consecutive` |
| `oscillator_range` | a tone | Sine's: `requested_hz`, `fs_hz` |
| `kinematic` | none | Random's: `channel`, `axis` (`accel`, `velocity` or `displacement`), `value`, `limit` |
| `drive_vpk` | none | Sine's: `vpk`, `abort_vpk` (the summed drive) |
| `random_vrms` | none | `realized_vrms`, `limit_vrms`: the random part's rms (floor and narrowbands) over the last quarter second reached `control.drive_abort_vrms` |
| `watchdog` | none | Sine's: `channel`, `axis`, `value`, `limit`, `measured`, `drive_vpk` (a limit channel's own abort) |
| `overload` | none | `channel`: an overload on a control or limit channel (the measurement the loop steers by); on any other channel the overload latches the channel's `overload` indicator and the run goes on |
| `station` | none | `message`: the station's own words |

A `law_fault` trip's `reason` is Random's for the floor (`error`, `panicked`, `over_budget`,
`wrong_length`, `non_finite`, `negative`, `non_finite_level`, `over_memory`, `bad_diagnostics`)
and Sine's for a tone (`error`, `panicked`, `over_budget`, `over_memory`, `non_finite`,
`negative`, `undeclared_value`, `too_many_values`).

**Run log.** `mixed.log` and its event take Random's form, with the kinds Random's and Sine's
use (`law` for live parameter changes and unusable calls) and one more: `part`, a line each time
a part switches on or off, by its window, its `end`, or `mixed.part` (naming the client).

- `record` is Random's: absent until the run arms, then the run folder, whether it holds the
  streams, and a problem writing it in words, or null.

### Mixed Mode Run Folders

Every Mixed Mode run that arms leaves a run folder as Random's does (Run Folders, and
`docs/run-folder.md`, "What a Mixed Mode Run Saves"), named
`<YYYYMMDD-HHMMSS>_mixed_<run id>`: claimed at arm and never written over, `run.json`,
`table.json`, `setup.json` (the configuration as accepted; what it resolved to: the floor's
breakpoints in g²/Hz with their alarm and abort bands, every tone and narrowband with its window,
its sweep, dwell or follow, its level and its bands, the level schedule, the limits and aborts,
the control settings and the controller's own configuration; the derived values; feasibility)
and `law/law.json` (both roles' laws, their parameters and their guards) before the first sample,
or the arm is refused `record_unwritable`. Every `mixed.log` entry is appended as it is made,
with the station frame and structured `data` for each state, level, part, hold, law, trip,
override and operator action; every enabled input and the drive as the device accepted it are
streams from the identification on unless `record.streams` is false; the results (the floor's
reference and its bands on the run's lines, each limit's ceiling, the identification's transfer
function and coherence per control channel, the floor's control, residual and drive spectra at
each level change and at the end, each tone's tracked level, phase and frequency, each
narrowband's and the floor's level, as the run went, and every call of both laws) are written as
the run goes, off the control thread; `end.json` holds `end` as `mixed.state` reports it. A
failure to write the record is a log entry of kind `record` and the state's `record.problem`;
the run goes on.

### Mixed Mode Display

Event topics:

- `mixed.state` (text): the state object on every change of `state`, `phase`, `mode`,
  `level_db`, `held`, `alarm`, `limiting` or `end`, or of a part's `on`, `alarm` or `settled`.
- `mixed.log` (text): new log entries.
- `mixed.live` (binary, the **default subscription**, 5 times a second from the start of
  identification until the run has finished, then once more).
- `mixed.data` (binary, to one subscribed connection): `mixed.live`'s fields but `views`, and
  `plots`, at most `rate_hz` times a second, and once at once on subscribe.

```
{"run_id","source","class","state","phase","mode","t_s","elapsed_s","parts_s","level_db",
 "commanded_db","at_level_s","remaining_at_level_s","level_s","loop","tick","held","alarm",
 "limiting","parts":[row...],"total":{"rms_g","reference_rms_g","drive_vpk","drive_vrms"},
 "channels":[{"index":1,"description":"Control","role":"control","rms":7.18,"unit":"g",
              "overload":false}],
 "alarms":[{"kind":"envelope","part":"floor","lines":2,"worst_db":3.4},
           {"kind":"band","part":"t2","side":"high","error_db":1.7}],
 "law":{"floor":{"values":{...},"misses":0,"capped":0},
        "tones":[{"part":"t2","values":{"servo_step_db":0.02,...},"misses":0,"capped":0}]},
 "views":[control, residual, drive, run readings]}
```

- `t_s`, `elapsed_s` and `parts_s` are the state's, for the tick the frame's numbers are from;
  `level_db` the level the drive is at now and `commanded_db` the ladder's (they differ while
  ramping); `at_level_s`, `remaining_at_level_s` and `level_s` Random's; `loop` the floor loops
  and `tick` the control ticks since the floor engaged.
- `parts` are the state's rows, one per part in the parts' order, for this frame.
- `channels` lists every enabled input in table order as Random's: its rms over its last half
  second of samples (tones in, in its own unit; null until the first half second) and Random's
  input-range `overload` latch.
- `alarms` lists what holds now: `{"kind":"envelope","part","lines","worst_db"}` (the floor's
  clean lines outside its alarm band; `worst_db` its clean line furthest from the reference, dB, from the residual estimate at the level in force), `{"kind":"band","part","side","error_db"}` (a tone or a
  narrowband outside its alarm band) and `{"kind":"overload","channel"}`. An alarm warns; it
  does not stop the run.
- `law` is the floor law's values on its last call by declared key, and each tone's instance's,
  one row per tone that has a law instance, in the parts' order; null until the laws are built.
- `views`, in this order:
  1. **Control** (`spectrum`, x `Frequency` Hz log, y `PSD` g²/Hz log, `link` `frequency`):
     `Control` (`measured`; the composite control spectrum as estimated, tones in; absent until
     the first loop), `Reference` (`reference`; the floor plus each narrowband where it is now),
     `Alarm high` and `Alarm low` (`tolerance`), `Abort high` and `Abort low` (`abort`), at the
     last loop's level. Every series is on the band's lines of the grid the run uses, with
     `shape: "steps"` and its `edges`, exactly as Random's. `regions` mark the lines left out of
     the floor's average in the last loop, one `cut` per span, `label` naming the part
     ("Left out: Blade pass").
  2. **Residual** (`spectrum`, same axes and `link`): `Residual` (`measured`; the composite after
     each tone's tracked waveform is subtracted, what the floor and narrowbands are judged on),
     then the same reference, alarm and abort series and regions, stepped on the same lines.
  3. **Drive** (`spectrum`, y `PSD` V²/Hz log, same `link`): `Drive` (`drive`), the random part's
     drive spectrum (floor and narrowbands) as synthesized for the next segment, stepped on the
     same lines. The tones' drives are in `parts`.
  4. **Run** (`reading`): `Level` dB, `Elapsed` s, `Remaining` s (when known), `Total rms` and
     `Reference rms` g, `Drive peak` V, then one row per part that is on, labelled by its name:
     its frequency (a tone or narrowband), `Measured` against `Target` in its unit and `Error`
     dB; a part's rows read `pass` inside its alarm band and `warn` outside it, the rest `info`.

`mixed.subscribe` takes Random's form: `{"rate_hz","plots":[plot...]}` (`rate_hz` 0.5 to 10,
default 5; at most 64 plots, each `id` unique, 1 to 64 characters) or `{"default":true}` (back to
`mixed.live`); a key a quantity does not take is `bad_params` naming it. A plot is
`{"id","quantity","channels","parts","points"}`: `channels` applies to `response` and `time` (a
list of indexes, or `"all"`, `"control"` or `"limited"`, as Random's); `parts` applies to
`amplitude`, `frequency` and `level` (a list of part ids, or `"all"` (the default), `"tones"` or
`"bands"`), resolved against the run's parts on every frame in the parts' order, a part the
quantity does not take, or that the run does not have, left out, never refused; `points` applies
to `time` only (64 to 8192, default 1024). The result is `{"default","rate_hz","plots",
"quantities"}`, every default filled in, each plot keeping only the keys its quantity takes. In
`mixed.data` each plot is `{"id","quantity","channels","parts","views"}`, `channels` the indexes
it resolved to for `response` and `time` and `parts` the ids it resolved to for `amplitude`,
`frequency` and `level` (null for the quantities without them), and for those the views are in
that order, one per channel or part.

| `quantity` | Views | What they show |
|---|---|---|
| `control` | one `spectrum` | The live frame's **Control** view |
| `residual` | one `spectrum` | The live frame's **Residual** view |
| `drive` | one `spectrum` | The live frame's **Drive** view |
| `response` | one `spectrum` per resolved channel | Random's: that channel's PSD (tones in), stepped, and a limited channel's `Limit` (`limit`; a control channel too), its ceiling stepped on the band's lines its rows cover |
| `time` | one `spectrum` per resolved channel | Random's: the last loop's acquired samples, tones in |
| `coherence` | one `spectrum` | Random's: the identification's coherence on the band's lines |
| `identification` | **Identification Gain** (`spectrum`, x `Frequency` Hz log, y `Gain` log) | `Gain` (`measured`, stepped on the band's lines), `abs(H)` from the drive to the composite control point, control unit per volt, and `Trusted` (`reference`, stepped) on the lines the tones may trust, the rest absent |
| `amplitude` | one `spectrum` per resolved tone | Titled by the part's name, x `Time` s linear (station time `t_s`, the last 60 s, at most 1200 points, `link` `time`), y `Amplitude` g log: `Measured` (`measured`), its composite tracked amplitude per tick while settled; `Target` (`reference`); `Alarm high`, `Alarm low` (`tolerance`); `Abort high`, `Abort low` (`abort`); each `shape: "line"` |
| `frequency` | one `spectrum` per resolved tone or narrowband | Titled by the part's name, the same time axis, y `Frequency` Hz linear: `Frequency` (`reference`, `line`), the part's frequency or center per tick |
| `level` | one `spectrum` per resolved narrowband | Titled by the part's name, the same time axis with one point per loop, y `Level` g²/Hz log: `Measured` (`measured`), its moving-frame level; `Reference` (`reference`); `Alarm high`, `Alarm low` (`tolerance`) and `Abort high`, `Abort low` (`abort`) widened to its scatter at each loop's degrees of freedom; each `shape: "line"` |
| `law` | `reading`s | **Floor law**, then **Tone law: <name>** for each tone with a law instance, in the parts' order: each declared value on the last call by its label and unit, then `Unusable calls in a row` (`pass` at 0, `warn` above) and `Caps bound` |
| `readings` | one `reading` | The live frame's **Run** reading |

A plot is empty (`views` empty) until there is something to draw: `coherence` and
`identification` until the gate has run, the others until the floor has engaged; a part's views
are present from its first switch-on, and keep its last 60 s of history while it is off.

### Mixed Mode Laws

Mixed Mode has no law world of its own. The floor's law is chosen from `laws.list` with
`{"world":"single"}` and the tones' law from `laws.list` with `{"world":"sine"}`; their
entries, parameters, live keys and diagnostics are those worlds' own (see "Control Laws"). A
law of the wrong world for its role is refused `law_wrong_world` at `control.law.floor` or
`control.law.tones`.

An imported law drives hardware in a Mixed Mode run once it has a passed check for its exact
file in its own world (the `sine` world's check for a tone law, at the Sine check in force; a
`single` law has none) **and**
a passed **mixed check** in its role. `laws.check` with `{"mixed":"floor"}` or
`{"mixed":"tones"}` (beside `id` or `path`, and `params`) runs the mixed check set with the law
in that role and the built-in in the other; without `mixed`, `laws.check` is as before (a
`single` law is still `law_check_unsupported` there). Each case is a whole Mixed Mode run under
the core's guards on a simulated station, every case first asking that every call of the law
under check is answered, in time, with zero misses and zero refused outputs:
`floor_only` (a floor alone, against Random on the same plant), `fixed_tone_on_floor`,
`swept_tone_through_mode`, `two_tones_close`, `narrowband_fixed`, `narrowband_swept`,
`level_ladder` and `transport_delay` (a hardware station's lag). `requirement` states each
case's pass band in words, as the core sets it. A case's result:

```
{"name":"fixed tone on floor","case":"fixed_tone_on_floor","role":"tones",
 "plant":"...","requirement":"...","passed":true,"detail":"..."}
```

The result is `{"law","report":{"passed","recorded","role","tool_version","cases"}}`: `law` the
role's facts as `derived.law.<role>` states them, read again after the record is written;
`tool_version` the mixed check's version (`mixed-check 1`); `cases` one result per case in the
order above. `detail` states the case's measured figures in words, or the trip that failed it.

A law file's result is recorded against its SHA-256 for that role (`recorded` true), passed or
not, beside the file as `laws/<sha256>.mixed-floor.check.json` or
`laws/<sha256>.mixed-tones.check.json`; a built-in law is checked and not recorded (it is cleared
already). The law's own entry in `laws.list`, and its own world's check, are unchanged by a mixed
check. A record clears its file in its role only: a passed record with the check's present
version and its present cases, by name and in order; a record of the other role, of an earlier
version or case list, or for other bytes clears nothing, and the law is checked again.
`mixed.configure` reports in `derived.law.<role>.hardware` whether the law may drive hardware in
that role. `mixed.arm` on a hardware station refuses a law without both records
`law_not_checked`, naming the role in `detail.role` and, in the message, which check stands in
the way. Refusals of `laws.check` with `mixed` are its own, and `bad_params` for a role other
than `floor` or `tones`; a record that cannot be written is `law_check_not_recorded`.

## Time Waveform Replication

A time waveform replication (TWR) test reproduces a recorded time history at the control channel,
one whole record at a time, and closes the loop between records. The service prepares the
reference (resample, detrend, high-pass, scale, taper), places the section to replicate between
quiet pads on a record of a power-of-two length, checks its kinematics against the feasibility
ceilings, identifies the system with band-limited random at a stated drive in volts, inverts the
identified plant, and seeds the control law from it. Each play plays the whole record, measures
the response, aligns it, judges it and gates it at its level; after a play the service wants
corrected, the law proposes the next full-level drive and the service checks it before it plays.
The level ladder, the gate, the playback schedule and every abort are the service's, never the
law's. The drive is in volts throughout. The run is one test like any other: a TWR run, a Sine
run, an SRS shock run, a classical shock run, a Random run, a MIMO run, an Explorer run, a Recorder
run and a `sim.start` run exclude each other, each refused `run_active` while another is live. **Any connected screen may
stop or abort** (`twr.stop`, `twr.abort`); `twr.configure` (other than a dry run), `arm`,
`start`, `proceed`, `iterate`, `play`, `level`, `freeze`, `reset` and `clear` need control;
everything that only reads or only computes is open to every connection. While the service is
shutting down, `twr.arm`, `start`, `proceed`, `iterate`, `play` and `level` are refused
`shutting_down`; `twr.freeze`, `stop` and `abort` are not.

| Method | Params | From | Result |
|---|---|---|---|
| `twr.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived","views"}` |
| `twr.config` | `{}` | any | `{"run_id","config","derived","views"}` |
| `twr.reference.import` | `{"path"}` or `{"text"}`, and `{"fs_hz","column","units","name"}` (optional) | any | `{"reference","warnings","views"}` |
| `twr.reference.preview` | `{"reference","prep","section","pads","acquisition","correction","feasibility"}` (all but `reference` optional) | any | `{"derived","views"}` |
| `twr.arm` | `{"override_ack"}` (optional) | `configured`, `blocked` | state |
| `twr.start` | `{}` | `configured`, `armed` | state |
| `twr.proceed` | `{}` | `review` | state |
| `twr.iterate` | `{}` | `running`, in `manual`, between plays | state |
| `twr.play` | `{}` | `running`, in `manual`, between plays, the level gated | state |
| `twr.level` | `{"delta_db"}` (1 or -1) or `{"rung"}` (from 0) | `running` or `review` in `manual`, between plays; in `auto` only at `holding` | `{"level_db"}` |
| `twr.freeze` | `{"frozen"}` | `running` or `review` | `{"frozen"}`, at once |
| `twr.stop` | `{}` | any | state |
| `twr.abort` | `{}` | any | state |
| `twr.reset` | `{}` | `held`, or any but live | state (`configured`) |
| `twr.clear` | `{}` | any but live | state |
| `twr.state` | `{}` | any | state |
| `twr.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `twr.subscribe` | see "TWR Display" | any | as there |
| `twr.plays` | `{"since"}` (optional, default 0) | any | `{"plays":[row...],"next"}` |
| `twr.record` | `{"index","points"}` (`points` optional) | any | `{"play":row,"views"}` |

"state" in the Result column is the `twr.state` object below. A run is live while it is armed,
running, holding for review, held by the coherence hold or stopping. `twr.configure` replaces a
run that is not live; `twr.reset` configures the present run's configuration again on the present
table and station (from the coherence hold it first ends the held run, `operator_stop`), and is
the only release from the coherence hold; `twr.clear` forgets the run (`unconfigured`). A result
that carries views travels as a binary frame: `twr.configure` with `dry_run`, `twr.config`,
`twr.reference.import`, `twr.reference.preview` and `twr.record`.

### TWR References

A reference reaches the service by the path of its file on the computer that runs the test, or
as pasted text; it is never uploaded. **`twr.reference.import`** reads it, keeps the raw record
in the data folder (`twr/references/`) under its SHA-256, and answers with its facts. The format
follows the file's extension (`.csv`, `.tsv`, `.wav`, `.json`), else its content; pasted text is
`text` (or `json` when it is a JSON object). A delimited record (CSV, TSV, pasted text) is read a
row at a time, numbers separated by commas, tabs, semicolons or spaces; lines starting `#` and
header rows before the data are skipped. Without `fs_hz` its first column is time, evenly spaced
within 1% of its median step, and the rate follows from it; with `fs_hz` every column is data.
`column` (default 0) picks the data column of a delimited record, or the channel of a WAV file
(PCM16 or float32, its own rate); JSON is the reference record format and carries its own rate
and unit. `units` (default `g`) names the record's unit, which must be the control row's; `name`
(default: the file's name without its extension, or `pasted`) labels it. A record holds at least
2 and at most 8388608 samples, every one finite.

The SHA-256 is of the record as read, not of the file: its rate, its unit and its samples, in the
service's own byte layout. One file read at two columns is two records; the same samples pasted
and read from a file are one. A configuration that names the file it came from (`reference.path`)
is checked by reading that file again as it was imported and comparing the two hashes.

```
{"reference":{"sha256":"9f2c0e...","path":"/Users/me/Eigen_Data/references/r1.csv","name":"r1",
              "format":"csv","fs_hz":12800,"samples":51200,"duration_s":4,"units":"g",
              "peak":0.2,"rms":0.058},
 "warnings":[],
 "views":[reference]}
```

`views` is **Reference** (`spectrum`, x `Time` s linear, y in the record's unit: `Reference`
(`reference`), min/max-decimated to 2048 points). Refusals: `bad_params` (neither `path` nor
`text`, or both; a relative path; `fs_hz` not above 0; `column` not a whole number),
`reference_unreadable` (the file cannot be read, or its content is not a record: `message` names
the line or field).

**`twr.reference.preview`** prepares a stored reference as a configuration would (`reference` is
`{"sha256"}`; the others as in "TWR Configuration", their defaults when left out, the correction
band the default identification's when not given) and answers with what the run would
replicate, so a screen never repeats the preparation. Its refusals: `bad_params` naming the
parameter for a wrong shape (`reference.sha256` that is not 64 lowercase hexadecimal digits, a
`prep`, `section`, `pads`, `acquisition`, `correction` or `feasibility` of the wrong type), and
`config_invalid` with the items a configuration would get for the parts given:
`reference_unknown` at `reference.sha256` (no stored reference has it), `rate_mismatch`,
`section_outside_reference`, `record_too_long`, `highpass_above_band`, `band_above_grid` and
`out_of_range`, each at its field:

```
{"derived":{"grid":{"fs_hz":12800,"record_len":65536,"df_hz":0.1953125,"record_s":5.12,
                    "section_start":512,"section_len":51200,"actual":false},
            "section":{"peak":0.2,"rms":0.058,"duration_s":4},
            "kinematics":{"peak_accel":0.2,"peak_velocity_ips":0.041,"disp_pkpk_in":0.000059,
                          "kinematics_hp_hz":5},
            "coverage":{"band_hz":[20,2000],"ref_floor_db":50,"fraction":0.9996,
                        "error_floor_pct":2.0},
            "feasibility":{"verdict":"pass","message":"...",
                           "checks":[{"name":"disp_pkpk_in","value":0.000059,"limit":1.0,"pass":true}]}},
 "views":[section, spectrum, velocity, displacement]}
```

- `grid`: the record every play is played and measured as: the section between its pads,
  padded to a power of two, at the requested rate (`actual` false).
- `section`: the prepared section at full level. `kinematics`: its peak acceleration, peak
  velocity and peak-to-peak displacement, integrated after a high-pass at `kinematics_hp_hz`.
- `coverage`: the fraction of the section's spectral energy inside the correction band and above
  the reference floor, before identification (identification can only lower it, by its
  coherence), and the error floor it sets, `100 sqrt(1 - fraction)` percent.
- `feasibility`: the kinematics against the ceilings set (`pass`, `exceeded`, or
  `not_evaluated` when none is), each check named by its ceiling's field (`peak_accel`,
  `peak_velocity_ips`, `disp_pkpk_in`).
- `views`: **Section** (the prepared record on the grid, time), **Spectrum** (`abs(R)` on the
  record grid from 0 to half the rate, with the reference floor across the correction band),
  **Velocity** and **Displacement** (time), each min/max-decimated to 1024 points.

### TWR Configuration

```
{"title":"Field Transient R1",
 "reference":{"sha256":"9f2c0e...","path":"/Users/me/Eigen_Data/references/r1.csv"},
 "prep":{"auto_resample":true,"detrend":"mean","highpass_hz":5,"scale":1,"taper_frac":0.02},
 "section":{"start":0,"len":51200},
 "pads":{"pre":512,"post":1024},
 "acquisition":{"fs_hz":12800},
 "identification":{"excitation":"random","drive_vrms":0.05,"duration_s":4,"nfft":2048,
                   "band_hz":[20,2000],"min_mean_coherence":0.8,"review":true},
 "inverse":{"tikhonov_floor":0.05,"coherence_floor":0.9},
 "correction":{"band_hz":[20,2000],"ref_floor_db":50,"min_band_energy":0.9},
 "control":{"law":{"id":"twr_correct","params":{"beta":0.5}},
            "law_budget":0.25,"law_misses":3,"max_peak_rise_db":10,"max_bin_rise_db":10,
            "first_drive_ratio":2,"alignment_frames":1},
 "schedule":{"mode":"auto","levels_db":[-12,-9,-6,-3,0],"gate_pct":10,"max_iterations":8,
             "on_gate_fail":"abort"},
 "run":{"schedule":"duration","duration_s":60},
 "adaptation":{"during_playback":false},
 "limits":{"drive_vpk_ceiling":0.5,"drive_clip_vpk":0.8,"drive_vrms_limit":null,
           "response_rms_limit":null,"envelope_alarm_frac":0.25,"envelope_abort_frac":0.5,
           "envelope_debounce_samples":8,"divergence_plays":3,"divergence_rise_frac":0.1},
 "feasibility":{"peak_accel":null,"peak_velocity_ips":null,"disp_pkpk_in":1.0,
                "kinematics_hp_hz":5},
 "operator":"Operator","article":"Bracket","notes":"",
 "specification":{"document":"Example procedure","clause":"4.2"},"record":{"streams":true},
 "metadata":[{"key":"run_number","label":"Run Number","value":"7"}]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `reference.sha256` | (required) | The stored reference (`twr.reference.import`), by its SHA-256 |
| `reference.path` | null | The file it was imported from. When given, it must be a file this record was imported from and still read to it, as it was imported (`reference_changed`); the run plays the stored record either way |
| `prep.auto_resample` | true | Resample the reference to the station's rate when they differ (upward only; a reference above the rate is refused, `rate_mismatch`). Off, a rate difference is refused (`rate_mismatch`) |
| `prep.detrend` | `mean` | `none`, `mean` or `linear` |
| `prep.highpass_hz` | 5 | A drift high-pass, Hz, above 0 and at most the correction band's low edge; null for none |
| `prep.scale` | 1 | Multiplies the reference (a unit correction); finite, not 0 |
| `prep.taper_frac` | 0.02 | A cosine taper at each end, a fraction of the reference (above 0, at most 0.5); null for none |
| `section` | the whole reference | The samples of the prepared reference to replicate: `start` and `len` |
| `pads.pre`, `pads.post` | 512, 1024 | Quiet samples before and after the section on the record (drive pre-ring and ring-down room). The record is the section and the pads padded to a power of two, at most 1048576 samples (`record_too_long`) |
| `acquisition.fs_hz` | the reference's rate | The rate requested of the station. The record and its section are prepared at it, and a station that runs at another rate is refused at arm (`arm_refused`); the run reports the rate it ran at |
| `identification.excitation` | `random` | `random` (band-limited, flat) or `log_chirp` |
| `identification.drive_vrms` | 0.05 | Its drive, volts rms; its peak is held under the drive ceiling and the clip |
| `identification.duration_s`, `nfft` | 4, 2048 | Its length, seconds, and the block it is estimated in (a power of two, at least 64, at most half the samples) |
| `identification.band_hz` | [20, 2000] | The band it excites, below half the rate |
| `identification.min_mean_coherence` | 0.8 | A mean in-band coherence under it holds the run (`held`, the coherence hold) |
| `identification.review` | true | Hold for the operator after a passing identification (`review`) |
| `inverse.tikhonov_floor` | 0.05 | The inverse's floor, a fraction of the in-band peak gain (above 0, below 1) |
| `inverse.coherence_floor` | 0.9 | Bins identified under this coherence are not trusted (0 to 1) |
| `correction.band_hz` | as `identification.band_hz` | The correction band, inside the identification band |
| `correction.ref_floor_db` | 50 | Bins of the reference more than this far under its in-band peak are not trusted |
| `correction.min_band_energy` | 0.9 | The fraction of the reference's energy the trusted bins must cover, or the run ends before any play (`aborted`, `coverage`); above 0, below 1 |
| `control.law` | `{"id":"twr_correct"}` | The control law, by its catalog `id` (see "Control Laws", world `twr`), and values for the parameters its manifest declares (`params` by key; defaults fill the rest) |
| `control.law_budget` | 0.25 | One law call's time, a fraction of the longer of the record and 4 s (`derived.call_budget_s`); above 0, at most 1 |
| `control.law_misses` | 3 | Unusable calls in a row that end the run `law_fault`; 1 to 100 |
| `control.max_peak_rise_db`, `max_bin_rise_db` | 10, 10 | The most the drive's peak, and any bin of it, may rise from one call to the next at full level, dB (above 0, at most 40; bins more than 40 dB under the largest are held to that floor first). With `twr_correct`, each is at least its `max_db_per_iter` plus 1 |
| `control.first_drive_ratio` | 2 | The seed's predicted response peak is at most this times the level target's (1 to 10) |
| `control.alignment_frames` | 1 | The most a play's fitted lag may drift from the identification's, frames, before the run ends (`aborted`, `alignment`); above 0, at most 16 |
| `schedule.mode` | `auto` | `auto` walks the levels, gating each; `manual` waits for `twr.iterate`, `twr.play` and `twr.level` |
| `schedule.levels_db` | [-12, -9, -6, -3, 0] | The level table, dB, non-decreasing, from -60 to 12; the last is full level |
| `schedule.gate_pct` | 10 | A play gates its level at an rms error at most this, percent (above 0, at most 100) |
| `schedule.max_iterations` | 8 | Correction plays at one level without a gate before `on_gate_fail` applies; 1 to 100 |
| `schedule.on_gate_fail` | `abort` | `abort` ends the run `not_converged`; `hold` holds at the level (`holding`) for a level verb or an abort |
| `run.schedule` | `once` | The playback once full level has gated: `once`, `loop` (`count` plays, 1 to 100000) or `duration` (`duration_s` seconds of section time, above 0, at most 86400: the section played `duration_s` over its length times, rounded up) |
| `adaptation.during_playback` | false | Keep correcting after each playback play (the law is called then too) |
| `limits.drive_vpk_ceiling` | (required) | The drive ceiling, volts peak (above 0, at most 10). A drive whose peak is over it is refused and the run aborts (`drive_vpk`); it is never clipped to fit |
| `limits.drive_clip_vpk` | null | A per-sample clip, volts; the law sees the drive as clipped (`limited`) |
| `limits.drive_vrms_limit` | null | A drive whose rms is over it is refused and the run aborts (`drive_rms`) |
| `limits.response_rms_limit` | null | A response whose rms is over it aborts the run (`response_rms`), in the control unit |
| `limits.envelope_alarm_frac`, `envelope_abort_frac` | 0.25, 0.5 | The response's band around the level target, a fraction of the target's peak: outside the alarm band raises the alarm, outside the abort band for `envelope_debounce_samples` samples in a row aborts (`envelope`), during a play or after it. Alarm at most abort |
| `limits.envelope_debounce_samples` | 8 | 1 to 10000 |
| `limits.divergence_plays`, `divergence_rise_frac` | 3, 0.1 | Correction plays in a row at one level, above the gate, each with its error up more than this fraction of the last, that end the run (`aborted`, `divergence`) |
| `feasibility.peak_accel`, `peak_velocity_ips`, `disp_pkpk_in` | null | Ceilings on the prepared section's kinematics at full level (control unit, in/s, in pk-pk). Over any, arming is blocked until acknowledged (`feasibility_exceeded`); null for none |
| `feasibility.kinematics_hp_hz` | 5 | The high-pass the kinematics are integrated after, Hz |
| `operator`, `article`, `notes`, `specification`, `record` | empty, null, `{"streams":true}` | The run's record, as Random's (see "Random Test Configuration" and "Run Folders"): the operator's words, what the test is run to, and whether the record holds the inputs and the drive as streams. None of them changes how the run drives. Every TWR run is recorded in its run folder (`docs/run-folder.md`, "What a TWR Run Saves"): the reference as imported, its target at full level, the identification, each play's shot of every enabled input and of the drive with the play's results as it is judged (the play the run ends on, and one cut short, included), and every call of the law with the drive it returned. `twr.record` answers from the service's memory, for the last eight plays |
| `metadata` | as shown | Random's |

A configuration that gives `capture` is refused `bad_params` at `config.capture`: every TWR run is
recorded in its run folder (see "Run Folders"), and the module has no capture of its own.

The control channel is the table's first enabled `control` row; the drive is its first enabled
`drive` row. A configuration whose shape is wrong is refused `bad_params` naming the field. Its
content errors are refused `config_invalid` with the items, located by `field`; `dry_run: true`
checks without replacing anything and needs no control.

**Result.** `state` is the `twr.state` object (`configured`). `derived` is the preview's
(`grid`, `section`, `kinematics`, `coverage`, `feasibility`), and:

```
{"reference":{"sha256":"9f2c0e...","name":"r1","fs_hz":12800,"units":"g","resampled":false},
 "call_budget_s":1.28,"plays_target":15,"identification_s":4}
```

`reference` is the stored record the run plays and whether it is resampled to the grid's rate;
`call_budget_s` one law call's time; `plays_target` the playback plays the run schedule asks for;
`identification_s` the identification's length. `views` are the preview's.

Item reasons (TWR): errors `out_of_range`, `reference_unknown` (no stored reference has that
SHA-256), `reference_changed` (the file at `reference.path` no longer has it), `units_mismatch`
(the reference's unit is not the control row's), `rate_mismatch`, `section_outside_reference`,
`record_too_long`, `band_above_grid`, `band_outside_identification` (the correction band leaves
the identification band), `highpass_above_band`, `identification_too_short` (fewer than two
blocks), `levels_empty`, `levels_not_ascending`, `alarm_wider_than_abort`, `no_control_channel`,
`no_drive_channel`, `law_unknown`, `law_wrong_world`, `law_param_unknown`, `law_param_invalid`;
warnings `reference_resampled`.

### Running a TWR Test

- **`twr.arm`** builds the station, prepares the record at the rate the run asks
  (`acquisition.fs_hz`) and checks feasibility. A section over a feasibility ceiling is refused
  `feasibility_exceeded` and the state becomes `blocked`; `twr.arm` with a non-empty
  `override_ack` arms past it (`override_not_needed` when nothing blocks). An imported law without
  a passed check for its exact file is refused `law_not_checked` on a hardware station, as is one
  whose passed check is of an earlier check version than the one in force (its `detail` names
  both: `{"tool_version","current"}`). Then the station is opened at the rate asked, the rate it
  programs is adopted and its arm gate passed. Nothing is driven. A station that programs another
  rate than the record's is refused `arm_refused` (the message names both rates), as is one the
  driver cannot open; the run's state is unchanged. The station's other refusals are Random's. The
  run's folder is claimed then (see "Run Folders"; refused `record_unwritable` when it cannot be
  written, and nothing is armed). The armed `grid` (`actual` true) is the station's rate and the
  record at it.
- **`twr.start`** from `configured` arms first. It runs identification (`phase` `sysid`): the
  excitation at `drive_vrms`, measuring the plant and its coherence on the identification grid,
  and the response's lag behind the drive by cross-correlation. A mean in-band coherence under
  `min_mean_coherence`, or a correlation with no peak, holds the run (`held`, phase
  `coherence_hold`): nothing drives, `twr.proceed` is refused `coherence_hold`, and `twr.reset`
  alone releases it. Trusted bins covering less than `min_band_energy` of the reference end the
  run (`aborted`, `coverage`). Otherwise it holds for review (`review`) or, without one, goes on
  as `twr.proceed` does. It answers at once.
- **`twr.proceed`** builds the law and seeds it (a law that cannot be built, or a seed that is
  not usable, ends the run `law_fault` before any play; a seed whose predicted response peak is
  over `first_drive_ratio` times the level target's is refused, `first_drive`). In `auto`, if the
  service's own prediction of the full-level drive (the inverse applied to the full-level
  reference) is over the drive ceiling, the run ends `aborted` (`drive_vpk`, `predicted: true`)
  before any play; then it plays the ladder. In `manual` it waits for `twr.iterate`.
- **Each play** scales the drive in force to the level, clips it (when `drive_clip_vpk` is set),
  tapers the outer half of each pad to zero so the play starts and ends at rest, and refuses a
  drive over the ceiling. It plays the whole record; each block is checked as it arrives against
  the abort band, and the debounce count outside it cancels the play with the station's own stop
  (`aborted`, `envelope`, `in_play: true`; the play has no row). After the play the response is
  aligned at the run's lag, and the play's own lag is fitted by cross-correlating the response
  with the response the identified plant predicts to the drive as played, on the trusted bins,
  within 16 frames either way: a
  drift over `alignment_frames` ends the run (`aborted`, `alignment`) before any correction from
  that play. Then the play is judged (the metrics, the envelope, the response rms) and gated at
  its level.
- **The law is called** after a correction play that did not gate, unless adaptation is frozen,
  and after a playback play when `adaptation.during_playback` is on. Its drive is checked: the
  record's length, every sample finite, the values declared and at most 8, the peak and bin rises
  against `max_peak_rise_db` and `max_bin_rise_db`, and the drive's predicted response peak at
  most `1 + envelope_abort_frac` times the level target's. A late, failed, panicked or refused
  call is a miss: the next play replays the drive in force; a usable call resets the count;
  `law_misses` in a row end the run (`law_fault`) and the law is not called again.
- **In `auto`** a play that gates moves to the next level, the drive carried forward scaled by
  the step; once full level gates, playback runs `run.schedule`. A level that spends
  `max_iterations` correction plays without a gate ends the run `not_converged`, or with `hold`
  holds it (`phase` `holding`) for `twr.level` or an abort.
- **`twr.iterate`** plays one correction play at the present level; **`twr.play`** begins
  playback from it, refused `not_converged` until the present level has gated, and the playback
  schedule then plays out as in `auto`. Both are refused `wrong_mode` in `auto`, `wrong_state`
  while a play is in flight or once playback has begun, and `coherence_hold` at the coherence
  hold.
- **`twr.level`** steps ±1 dB (`delta_db`) or goes to a level of the table (`rung`, from 0),
  between plays and before playback, the drive carried forward by the exact ratio; from
  `holding` it releases the hold and the run goes on from the new level. From `review` it steps
  from the table's first level. In `auto` it is taken only at `holding` (a level that failed its
  gate under `hold`); anywhere else in `auto` it is refused `wrong_mode`. Refused `wrong_state`
  while a play is in flight,
  `not_a_rung` (a rung the table does not have, or a step past its first or last level), and
  `drive_ceiling` when the drive's peak at the new level would pass the ceiling (with `vpk`,
  null when the service cannot state it, `ceiling_vpk` and `level_db`); the level and the drive
  are then unchanged.
- **`twr.freeze`** with `frozen` true stops the law's corrections (the drive in force replays);
  false resumes them. It answers at once with `frozen` as asked, takes effect before the next play
  (a play in flight is corrected after as it would have been) and is logged; `state.frozen`
  follows.
- **`twr.stop`** cancels the play in flight at the next block: what the station already holds
  plays out (at most its lookahead and a block), then the drive falls along a 250 ms raised
  cosine, and the run ends `operator_stop` with `end.frames_done`. Between plays, or from `armed`,
  `review` or `held`, it ends at once. **`twr.abort`** does the same with a 25 ms taper and ends
  `operator_abort`. Both answer at once, and nothing plays after either.
- **When nobody holds control** no new play starts; the play in flight finishes; a claim within
  `timeout_s` resumes the run, and otherwise a play still in flight is cancelled with the stop's
  taper and the run ends `safe_stop`. The same holds at `review` and at the coherence hold.

**Time is the station's.** `t_s`, each play's `at_s` and `end.at_s` count the frames the station
clocked over every play and pad since the run armed, at the rate it adopted, never the service's
clock. The safe stop's countdown (`orphan_s`) is the only number timed by the service's clock.

### TWR Run State

```
{"run_id":"twr-0002","state":"running","phase":"playing","title":"Field Transient R1",
 "source":"SIM","station":{"kind":"sim","class":"sim","devices":[]},
 "grid":{"fs_hz":12800,"record_len":65536,"df_hz":0.1953125,"record_s":5.12,
         "section_start":512,"section_len":51200,"actual":true},"clock":"shared",
 "mode":"auto","levels_db":[-12,-9,-6,-3,0],"level_db":0,"rung":4,
 "plays":{"done":8,"at_level":1,"of_level":8,"playback":2,"of_playback":15},
 "frozen":false,"t_s":61.4,"alarm":false,
 "identification":{"excitation":"random","drive_vrms":0.05,"drive_vpk":0.23,
                   "coherence_mean":0.9999,"min_mean_coherence":0.8,"coverage":0.9996,
                   "lag_frames":41,"lag_source":"measured","passed":true},
 "prediction":{"drive_vpk":0.2,"drive_vpk_ceiling":0.5,"level_db":0},
 "last_play":{row},
 "law":{"id":"twr_correct","name":"frequency-domain correction","version":"0.1.0",
        "sha256":null,"misses":0},
 "feasibility":{"verdict":"pass","message":"..."},"end":null}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `blocked` | `feasibility_blocked` | Feasibility exceeded; arming needs an acknowledgement |
| `armed` | `armed` | Station open and gated; nothing driven yet |
| `running` | `sysid`, `iterating`, `playing`, `waiting`, `holding` | Identification; a correction play; a playback play; between plays (in `manual` waiting for a verb, or paused while nobody holds control); a level that failed its gate under `hold` |
| `review` | `awaiting_review` | Identification passed; nothing plays until `twr.proceed` |
| `held` | `coherence_hold` | Identification did not reach its coherence; nothing drives until `twr.reset` |
| `stopping` | `stopping` | A stop or an abort is bringing the play in flight to rest |
| `finished` | `stopped`, `aborted` | Ended; `end` says how |

- `level_db` is the commanded level, null until `twr.proceed` (or a level move from `review`);
  `rung` its place in `levels_db` (null when a manual step has left the table's levels).
- `plays`: `done` the plays judged over the run (identification not counted); `at_level` the
  correction plays judged at this level (not the one in flight) and `of_level` the most it may
  take (`max_iterations`); `playback` the playback plays done and `of_playback` the schedule's
  (`derived.plays_target`).
- `frozen` is true while `twr.freeze` holds the law's corrections off. `t_s` is station time.
  `alarm` is true once a play's response has left the alarm band (latched for the run).
- `identification` and `prediction` are null until identification is done. `identification` is
  the drive it played (rms and peak), its mean in-band coherence against the hold, the trusted
  bins' coverage of the reference, and the lag it aligns every play at, with `lag_source`
  `measured` or `derived`. At the coherence hold `coverage` is null and `coherence_mean` is
  the mean the identification measured, null when the hold is the lag's (no correlation peak, so
  no coherence was measured); `lag_frames` is null when no lag was found.
  `prediction` is the peak of the drive in force at the present level against the ceiling: it
  follows what the law changed, never the seed alone, and is null while the service cannot state
  it.
- `last_play` is the last play's row (below); `law` the run's law and its unusable calls in a row
  now; `feasibility` is null until arm.
- `end` is null until the run finishes, then `{"outcome","message","at_s","level_db","play",
  "frames_done","trip"}`: `at_s` station time, `play` the play it ended at (the last judged, or
  the one a stop or an abort cancelled; null before any), `frames_done` the frames of that play
  the station had taken when a stop or an abort cancelled it (null otherwise).
- `record` is absent until the run arms, then Random's `{"folder","streams","problem"}`.

| `outcome` | How the run ended |
|---|---|
| `completed` | The playback schedule finished |
| `operator_stop` | `twr.stop` |
| `operator_abort` | `twr.abort` |
| `safe_stop` | Nobody held control for `timeout_s`, or the service was shut down; `message` says which |
| `aborted` | A protection tripped; `trip` says which |
| `not_converged` | A level spent `max_iterations` correction plays without a gate, under `on_gate_fail` `abort` |
| `law_fault` | The law gave no usable drive `law_misses` times in a row, or could not be built or seeded; the drive in force replayed through the misses, then the run ended. `trip` says why |
| `fault` | The run or the station failed, or the delivery path returned no response to judge; `message` is the controller's words |

`trip` is null unless `outcome` is `aborted`, `not_converged` or `law_fault`; it has `kind`,
`play` (the row the trip judged, or for a play the in-play envelope check cut short the index it
would have had; null at identification and when a drive was refused before it played),
`at_level_db` and its kind's facts. `predicted: true` marks a trip found on a prediction
before a play, so nothing was driven for it. A law whose first drive is refused ends
`law_fault` with the reason `first_drive`.

| `kind` | Facts |
|---|---|
| `envelope` | `in_play` (found during the play, from a block), `worst_frac`, `abort_frac`, `samples`, `at_sample` (the record sample it tripped at) |
| `drive_vpk` | `vpk`, `ceiling_vpk`, `predicted` |
| `drive_rms` | `vrms`, `limit_vrms` |
| `response_rms` | `rms`, `limit` |
| `alignment` | `lag_frames` (the play's fitted lag), `identification_lag_frames`, `drift_frames`, `limit_frames` |
| `coverage` | `coverage`, `min_band_energy`, `error_floor_pct` |
| `divergence` | `plays`, `error_pct` |
| `not_converged` | `iterations`, `error_pct`, `gate_pct` |
| `law_fault` | `reason`, `consecutive` |

A `law_fault` trip's `reason` is `error` (the law reported one, or could not be built),
`panicked` (it panicked or, for a law file, trapped), `over_budget`, `over_memory` (a law file),
`wrong_length`, `non_finite`, `undeclared_value`, `too_many_values`, `peak_rise`, `bin_rise` (the
per-call caps), `predicted_peak` (the drive's predicted response peak over its bound), or
`first_drive` (the seed's predicted response peak over its bound; nothing played).

### TWR Plays

Every play after the identification has a row. `twr.plays` returns the rows from `since` (the row
`index`, from 0) and `next`, the index after the last. `twr.record` returns one row and its
records as views: **Replication** (the level target and the response), **Error** (the response
less the target) and **Drive** (the drive as played), each over the record from its first
sample, min/max-decimated to `points` points (default 1024, 64 to 8192; `0` sends every sample).
The records of the most recent 8 plays are held; an older play's are refused `not_found`, its
row is still in `twr.plays`.

```
{"index":7,"kind":"iteration","level_db":0,"rung":4,"iteration":1,
 "rms_error_pct":1.6,"peak_error_pct":4.2,"spectral_error_db":0.11,"gated":true,
 "clipped_samples":0,"clamped_bins":0,"drive_vpk":0.2,"response_peak":0.2,
 "lag_frames":41.02,"drift_frames":0.02,"start_frame":409600,"frames":66560,"at_s":32,
 "law":{"used":true,"miss":null,"call_ms":3.2,"peak_rise_db":0.4,"bin_rise_db":1.1,
        "predicted_peak_ratio":1.01,"values":{"corrected_bins":812,"clamped_bins":0,
                                              "drive_change_db":0.2}}}
```

- `kind` is `iteration` (a correction play on the ladder) or `playback`; `level_db` and `rung`
  where it played; `iteration` its count at that level (from 1; null for a playback play).
- `rms_error_pct`, `peak_error_pct`, `spectral_error_db` (over the trusted bins) and `gated` are
  the service's judgement; `clipped_samples` the samples the clip held, `clamped_bins` the bins
  the law's own growth limit or the service's cap held back. The rms and peak errors compare the
  response with the level target over the whole record, each about its mean: a constant on the
  measured input (a DC-coupled channel's offset) is not counted, since no correction acts at 0 Hz.
- `drive_vpk` and `response_peak` are the peaks as played and measured; `lag_frames` the play's
  fitted lag and `drift_frames` its drift from the identification's (the alignment check), each
  null until the run's alignment check has measured them.
- `start_frame` is the station frame the play started at and `frames` the frames the station
  clocked for it (the record and its rest pad); `at_s` its start in station time.
- A play that was played and measured has a row, the one a response check after the play ended
  the run on included (not gated). A drive refused before it played, a play the in-play envelope
  check cut short, and a play a stop or an abort cancelled, have none; their frames still count in
  station time.
- `law` is the call that produced the drive this play played (`used` false and `miss` the reason
  when the drive in force was replayed after a miss): its time, the peak and worst bin rise it
  asked for over the drive before it, dB, its predicted peak over the level target's, and the
  values it reported. Null for a play with no call behind it (a gated level carried forward, or
  a frozen run).

### TWR Display

Event topics:

- `twr.state` (text): the state object on every change: after every judged play (its
  `last_play`, so a screen can append the rows from it), on every change of state or phase, and
  after `twr.freeze` takes effect. The station time alone changing sends none.
- `twr.log` (text): new log entries, as Random's, driver warnings included, and after every
  judged play a note of the frames the station took before it started the play.
- `twr.live` (binary, to every connection without a TWR subscription of its own, as Random's and
  Sine's live frames, 5 times a second from arm until the run has finished, then once more):
  `{"run_id","source","state","phase","mode","t_s","level_db","rung",
  "plays","play","last","views"}`. `play` is the play in flight, `{"index","kind","iteration",
  "frames_done","frames"}`, or null between plays; `last` is the last play's
  `{"rms_error_pct","peak_error_pct","spectral_error_db","gated"}` (null before any); `views` is
  **Scope** (the control channel's most recent samples, from the station's display tap, x `Time`
  s, at most 1024 points).
- `twr.data` (binary, to one subscribed connection, in place of `twr.live`): `twr.live`'s fields
  but `views`, and `plots`: at its `rate_hz` while a play is in flight, whatever its plots show,
  so `play.frames_done` moves on it; once when identification ends, once after each play, once
  when the run has finished, and at once on subscribe (so a screen that joins late has the
  latest play).

`twr.subscribe` takes Random's form: `{"rate_hz","plots":[plot...]}` (`rate_hz` 0.5 to 10,
default 5; at most 64 plots, each `id` unique) or `{"default":true}` (no `twr.data`); a key a
quantity does not take is `bad_params` naming it. A plot is `{"id","quantity","points"}`; `points`
applies to `target`, `response`, `error`, `drive`, `envelope` and `scope` only (64 to 8192,
default 1024; records are min/max-decimated to it). The result is
`{"default","rate_hz","plots","quantities"}`, every default filled in. In `twr.data` each plot
comes back as `{"id","quantity","views"}`.

| `quantity` | Views | What they show |
|---|---|---|
| `target` | one `spectrum` (x `Time` s) | **Target**: the level target over the record, control unit |
| `response` | one `spectrum` (time) | **Response**: the last play's response (`measured`) with the level target (`reference`) |
| `error` | one `spectrum` (time) | **Error**: the last play's response less the target |
| `drive` | one `spectrum` (time) | **Drive**: the last play's drive as played, V |
| `envelope` | one `spectrum` (time) | **Envelope**: the last play's response with the alarm (`tolerance`) and abort (`abort`) bands around the target |
| `frf` | **Identification Gain**, **Identification Phase** (`spectrum`s, x `Frequency` Hz log) | `abs(H)` (control unit per volt) and its phase, degrees, on the identification grid in its band |
| `coherence` | one `spectrum` | **Coherence**: the identification's coherence with `coherence_floor` and `min_mean_coherence` |
| `mask` | one `spectrum` | **Trust**: the trusted bins on the record grid (1 trusted, 0 not), with the reference spectrum, scaled to 1 at its peak |
| `spectra` | one `spectrum` | **Spectra**: `abs(R)` at the level, `abs(Y)` of the last play and `abs(D)` of its drive on the record grid, in the correction band |
| `convergence` | one `spectrum` | **Convergence**: each play's rms error, percent, against the play index, with the gate |
| `law` | **Law** (`spectrum`), **Law Values** (`reading`) | Each declared value against the play index of its call; the last used call's values by their labels, and the unusable calls in a row |
| `scope` | one `spectrum` (time) | **Scope**: the control channel during the play in flight |
| `readings` | one `reading` | The run's numbers |
| `plays` | one `table` | The play rows |

A plot of the last play is empty (`views` empty) until a play has been judged, `frf`,
`coherence` and `mask` until identification has ended, `law` until the law has been called, and
`scope` between plays.

## Explorer

An Explorer test drives one sine tone that the operator steers by hand: its frequency, its level,
a sweep toward either edge of the band, and a stop. Nothing closes a loop: the service measures
the response and never corrects the drive from it, so the operator is the loop. The drive is in
volts peak at the drive output, from 0 V up to the ceiling set at configure, and the level is
never asked for in any other unit. The run starts quiet: the tone runs at exactly 0 V until the
operator raises the level, which is why Explorer has no review hold. While the tone moves the
service remembers the response level at each frequency it has visited, the survey line, from
which the operator reads a resonance. The run is one test like any other: an Explorer run, a TWR
run, a Sine run, an SRS shock run, a classical shock run, a Random run, a MIMO run, a Recorder run
and a `sim.start` run exclude each other, each refused `run_active` while another is live. **Any
connected screen may stop or abort** (`explorer.stop`, `explorer.abort`); `explorer.configure`
(other than a dry run), `start`, `steer`, `sweep`, `hold`, `survey.clear`, `reset` and `clear`
need control; everything that only reads or only computes is open to every connection. While the
service is shutting down, `explorer.start`, `steer` and `sweep` are refused `shutting_down`;
`explorer.hold`, `stop` and `abort` are not.

| Method | Params | From | Result |
|---|---|---|---|
| `explorer.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived"}` |
| `explorer.config` | `{}` | any | `{"run_id","config","derived"}` |
| `explorer.start` | `{}` | `configured` | state, once the tone runs at 0 V |
| `explorer.steer` | a frequency part (`{"hz"}`, `{"step_hz"}` or `{"step_oct"}`), a level part (`{"vpk"}` or `{"step_db"}`), or one of each | `running` | `{"hz","vpk","clamped","frame","input_frame"}` |
| `explorer.sweep` | `{"direction","rate_oct_per_min"}` | `running` | `{"direction","rate_oct_per_min","clamped","frame","input_frame"}` |
| `explorer.hold` | `{}` | `running` | `{"hz","frame","input_frame"}` |
| `explorer.stop` | `{}` | any | state |
| `explorer.abort` | `{}` | any | state |
| `explorer.survey` | `{"since"}` (optional) | any | `{"revision","channel","unit","points","peak"}` |
| `explorer.survey.clear` | `{}` | `running`, `stopping` | `{"revision"}` |
| `explorer.reset` | `{}` | any but live | state |
| `explorer.clear` | `{}` | any but live | state |
| `explorer.state` | `{}` | any | state |
| `explorer.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `explorer.subscribe` | see "Explorer Display" | any | as there |

"state" in the Result column is the `explorer.state` object below. A run is live while it is
starting, running or stopping. `explorer.configure` replaces a run that is not live;
`explorer.reset` returns an ended run to `configured` with its configuration on the present table
and station, the survey line and the log empty; `explorer.clear` forgets it (`unconfigured`). While
a run is live both are refused `run_active` (naming that run), and so is `explorer.reset` while
another module's run is live, as a configure would be. `explorer.start` on a run already started
is refused `wrong_state`.

### Explorer Configuration

```
{"title":"Survey, fixture A",
 "acquisition":{"fs_hz":12800},
 "band_hz":[20,2000],"start_hz":250,"max_drive_vpk":0.5,
 "slew":{"freq_oct_per_s":1,"drive_db_per_s":6},
 "display":{"nfft":4096},
 "capture":{"run_name":"explorer"},
 "operator":"A. Operator","article":"Bracket, SN 0042","notes":"",
 "specification":{"document":"Test procedure","clause":"4.2"},
 "record":{"streams":true},
 "metadata":[{"key":"run_number","label":"Run Number","value":"3"}]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `acquisition.fs_hz` | null: 8192 | The rate requested of the station, 1000 to 204800 Hz. A station may program another rate; the run adopts it, derives every per-frame number from it and reports both (`grid`) |
| `band_hz` | `[10,2000]` | The band the tone may move in, `[lo, hi]` Hz, above 0 and ascending, `hi` under 0.45 times the requested rate (`band_above_grid`). Once armed it is narrowed under 0.45 times the rate the station programmed (logged) |
| `start_hz` | 100 | Where the tone stands at start, inside the band |
| `max_drive_vpk` | (required) | The drive ceiling, volts peak: above 0, at most 10, and at most the drive row's `range_max` (`ceiling_over_range`). Every level is clamped to it, and a generated sample over it aborts the run (`drive_vpk`) |
| `slew.freq_oct_per_s` | 1 | The frequency slew, octaves per second (above 0, at most 10): the fastest a sweep moves the tone. A sweep is answered at no more than 60 times it, in octaves per minute |
| `slew.drive_db_per_s` | 6 | The drive slew, dB per second (above 0, at most 1000): every level change and the stop ramp at it. A slew whose 40 dB stop would outlast the service's stop wait (`derived.stop_wait_s`, under 1.333 dB/s at 30 s) is refused (`safe_stop_budget`) |
| `display.nfft` | 4096 | The display spectra's FFT size, a power of two from 1024 to 16384. The rate over it is the spectra's line spacing, and half of that the survey's tolerance: a finer FFT gives a finer survey line |
| `capture.run_name` | `explorer` | The run folder's name under the local data root (see "Explorer Records"), a plain name with no path in it |
| `metadata` | the default fields | Random's: `{"key","label","value"}`, `label` the key when absent |
| `operator`, `article`, `notes` | empty | Random's: who ran the run, on what, and notes, for the run's record |
| `specification` | null | Random's: `{"document","clause"}`, the specification the run was made to |
| `record.streams` | true | Whether the run's record keeps its time histories: every input as read and the drive as the device accepted it |

The drive is the table's one enabled `drive` row; the acquired channels are every other enabled
row, in table order; the **control row** (the one the survey line, the measured frequency and the
derived motion are read from) is the first enabled `control` row, else the first enabled input.
The table must have exactly one enabled drive row and at least one enabled input
(`no_drive_channel`, `extra_drive_channels`, `no_input_channel`). Each row's `abort` limits are the
run's abort limits. A configuration whose shape is wrong is refused `bad_params` naming the field
in `detail.param` as the request's own path (`config.slew.drive_db_per_s`), and so is a key the
configuration does not have, so no level in another unit can be asked for; its content errors are
refused `config_invalid` with the items, located by `field`, the path inside the configuration
(`band_hz`, with no `config.` before it). `dry_run: true` checks without replacing anything and
needs no control.

Item reasons (Explorer): errors `out_of_range`, `band_above_grid`, `ceiling_over_range`,
`safe_stop_budget`, `no_drive_channel`, `extra_drive_channels` (more than one enabled drive row:
the extra outputs would stay silent), `no_input_channel` (nothing would measure the tone); no
warnings.

**Result.** `state` is the `explorer.state` object (`configured`). `derived`, here for an NI
station at 12 800 Hz:

```
{"grid":{"fs_hz":12800,"block_frames":256,"lookahead_frames":512,"actual":false},
 "band_hz":[20,2000],"pipeline_frames":1063,"pipeline_s":0.08305,
 "rise_s":6.667,"stop_s":6.667,"stop_wait_s":30,"sweep_max_oct_per_min":60,
 "display":{"df_hz":3.125,"window_s":0.32},
 "survey":{"tolerance_hz":1.5625,"first_point_s":[0.383,0.125]}}
```

- `grid`: the requested rate, and the block and the output lookahead (frames) the selected station
  works in at it; `actual` is true once the run has armed and all three are the station's. The
  block is the power of two nearest 20 ms of the rate. Before the station is opened the lookahead
  and the input group delay are the figures the station states: the simulator's 4 frames (or the
  lookahead a simulated station states) with no group delay, or an NI station's two blocks and its
  input path's documented group delay, 38.4 + 3.2e-6 times the rate in samples, rounded up (39
  frames at 12 800 Hz). The run state carries the armed station's own.
- `band_hz`: the band as it stands at the requested rate; a station that programs another rate
  may narrow it at start.
- `pipeline_frames` and `pipeline_s`: how many input frames still carry the tone from before a
  change at the moment the change is made: the drive the start writes ahead into the station's
  output and into the service's own queue (two lookaheads), plus the station's input group delay
  where it states one. The survey line and the measured frequency wait it out after every change.
  The run state carries the armed station's own figure.
- `rise_s` and `stop_s`: how long a rise from 0 V and the stop take at the drive slew: 40 dB at
  `slew.drive_db_per_s`, then the step to 0 V (or, rising, the step from 0 V to 1% of the level
  first). `stop_wait_s` is how long the service waits for a stop, which the stop must fit inside.
- `sweep_max_oct_per_min`: the fastest sweep, 60 times `slew.freq_oct_per_s`.
- `display`: the spectra's line spacing and the length of one window, seconds.
- `survey`: the tolerance (a visit that close to a point replaces it), and how long after a change
  of frequency the first point at the new frequency comes, at the band's low and high edges: the
  pipeline, the guard (two blocks, or three cycles of the new tone if longer), then three cycles.
  The point itself can come up to one batch later than that.

### Running an Explorer Test

- **`explorer.start`** opens the station at `acquisition.fs_hz`, adopts the rate it programs,
  narrows the band under 0.45 times that rate (logged), passes the arm gate, writes the station's
  lead-in at 0 V and starts the clock and the tone in one step (`starting`, `priming`, while the
  station opens). It answers once the tone runs at 0 V at `start_hz` (`running`, `quiet`).
  Refusals, in the order they are checked: `shutting_down`, `not_controller`, `not_configured`,
  `run_finished` (reset first), `wrong_state` (already started), `run_active` (another module's
  run is live; it names that run, so a live Sine run refuses an Explorer start and a live Explorer
  run refuses a Sine arm, each naming the other), `table_invalid`, `config_invalid` (the table, as
  it is now, no longer carries the run: the same items as configure), then, once the station is
  being opened, `devices_absent`, `hardware_busy`, `record_unwritable` (the run's record cannot
  be written in the data folder, naming the path and the reason, before the station is opened;
  see "Explorer Records") and `arm_refused` (the station or the arm gate refused; the message is
  the controller's own). A refusal once the station is being opened ends the run `arm_refused`,
  and `explorer.reset` comes before another start.
- **`explorer.steer`** moves the tone. A frequency part is `hz` (absolute), `step_hz` (added to
  the target frequency) or `step_oct` (the target frequency times 2 to that power); a level part
  is `vpk` (absolute) or `step_db` (from the target level). A steer has one part or one of each,
  never two of a kind (`bad_params`), and both parts land in the same generated frame. A frequency
  is clamped into the band and the tone lands on it in one frame without a break in phase;
  setting a frequency halts a sweep. A level is clamped to `[0, max_drive_vpk]` and the drive
  ramps to it at the drive slew: from 0 V it steps once to 1% of the level (40 dB under it), then
  ramps; down to 0 V it ramps to 1% of where it was, then steps to 0. A `step_db` while the target
  level is 0 V is refused `level_zero` and nothing of the steer is applied: set a level in volts
  first. The answer is what the service adopted: `hz` and `vpk` are the targets in force after
  the steer, `clamped` is `{"hz","vpk"}`, true where a part was clamped (false for a part not
  sent), `frame` is the first generated frame made under the change, and `input_frame` the input
  frames acquired when the service took it (see "Time is the station's"); the sweep and hold
  answers carry both too. A level moves the sample of `frame` itself. A retune keeps the phase, so
  `frame` is the first at the new frequency and its sample continues the old phase: the
  waveform's value first differs one frame later.
  Refused `wrong_state` outside `running`: before start, while starting, once a stop has begun
  (`state` `stopping`: nothing raises or moves the drive after that) and after the end.
- **`explorer.sweep`** sweeps the tone from where it stands toward the band's `up` or `down` edge
  at `rate_oct_per_min` (above 0), clamped to `derived.sweep_max_oct_per_min`; the answer is the
  rate used and whether it was clamped. At the edge the tone parks and holds (`at_edge`); a sweep
  never turns by itself. Another `explorer.sweep` re-aims it. Refused `wrong_state` outside
  `running`.
- **`explorer.hold`** ends the sweep and the tone holds where it is now (`hz` in the answer).
  Without a sweep it changes nothing and answers the target frequency in force. Refused
  `wrong_state` outside `running`.
- The steering verbs are taken between the service's batches of two blocks, and each answers
  within one batch and one analysis pass.
- **`explorer.stop`** is the controlled stop: any sweep ends, the drive ramps to 0 V at the drive
  slew (`stopping`, `ramping_down`), then the station's tasks stop and the run ends
  `operator_stop`. From `starting` it takes effect once the station has opened (the drive is
  still at 0 V, so the ramp is immediate). It answers at once, with the state as it stands.
- **`explorer.abort`** ends the run within one batch without the ramp; the station takes the drive
  down with its own stop. The outcome is `operator_abort`.
- **The protections.** Every generated sample is checked against `max_drive_vpk`: a sample over it
  is clipped and the run aborts (trip `drive_vpk`). Each enabled row's `abort` limits are checked
  on every input sample (trip `watchdog`). A second, slower reading of the control row's derived
  velocity and displacement against its limits, once the derived values have settled, ends the
  run with the controlled stop (`limit_stop`). An underrun, a stalled input, a lost device, a
  scheduling fault or a failure of the service's own loop ends the run at once (`fault`). A stall
  of that loop shows in the state (`stall`), with Abort open.
- **When nobody holds control** the run goes on unchanged, tone, level and any sweep (which parks
  at its edge); after `timeout_s` it ends `safe_stop` with the controlled stop, as Random's. While
  the service shuts down it ends the same way.

**Time is the station's.** `t_s` in the state and the live frames, `end.at_s` and `end.ramp_s`
count the station's own input frames at the rate it adopted (frames acquired since start over
`grid.fs_hz`), never the service's clock. The output and the input run on one sample clock and
start together, and the steering answers' `frame` (generated frames) and `input_frame`, and the
time plots' `first_frame` (input frames), count it from the start of the run: generated frame n
plays at the clock's n-th sample, and its response reaches the input at input frame n plus the
station's input group delay. So a change the service takes at `input_frame` reaches the input
`pipeline_frames` later, at `frame` plus the group delay. The safe stop's countdown (`orphan_s`) is the only number timed by the
service's clock.

### Explorer Run State

```
{"run_id":"explorer-0002","state":"running","phase":"steady","title":"Survey, fixture A",
 "source":"HARDWARE","station":{"kind":"ni","class":"ni_real","devices":[...]},
 "grid":{"requested_fs_hz":12800,"fs_hz":12800,"block_frames":256,"lookahead_frames":512,
         "actual":true},"clock":"shared",
 "band_hz":[20,2000],"max_drive_vpk":0.5,"slew":{"freq_oct_per_s":1,"drive_db_per_s":6},
 "pipeline_frames":1063,"t_s":41.38,
 "frequency_hz":250,"target_hz":250,"drive_vpk":0.2,"target_vpk":0.2,"sweep":null,
 "health":{"engine_ticks":2069,"worst_tick_ms":0.41,"budget_ms":20,"health_faults":0,
           "ring_overruns":0,"blocks_dropped":0,"driver_warnings":2,
           "pass_ms":{"last":1.9,"worst":3.4},"batch_gap_ms":4.1},
 "stall":null,"stop":null,"end":null,
 "record":{"folder":"/home/operator/Hertzforge_Data/explorer","streams":"on","problem":null}}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `starting` | `priming` | `explorer.start`: the station is opening and arming; nothing is driven |
| `running` | `quiet` | The tone runs at 0 V |
| `running` | `ramping` | The level is moving toward its target |
| `running` | `sweeping` | A sweep is moving the tone |
| `running` | `at_edge` | A sweep has parked at its band edge |
| `running` | `steady` | The tone holds its frequency and its level |
| `stopping` | `ramping_down` | The controlled stop is ramping the drive to 0 V |
| `finished` | `stopped`, `aborted`, `faulted` | Ended; `end` says how |

- One phase holds at a time: `quiet` while the target level and the drive are both 0 V; else
  `ramping` while the drive differs from its target, whatever the frequency does; else `sweeping`
  or `at_edge`; else `steady`. A finished run's phase is `stopped` after `operator_stop`,
  `safe_stop` or `limit_stop`, `aborted` after `operator_abort` or `aborted`, and `faulted` after
  `fault` or `arm_refused`.
- `grid`: `requested_fs_hz` is the rate asked; `fs_hz` the rate the station programmed once armed
  (the request until then); `block_frames` and `lookahead_frames` as in `derived`; `actual` true
  once armed. `clock` as Random's.
- `band_hz` is the band in force (narrowed once armed); `max_drive_vpk` and `slew` as configured.
- `pipeline_frames`: as in `derived` until the run has armed, then the armed station's own figure.
- `t_s`: station time since start; null before.
- `frequency_hz` and `drive_vpk` are the tone now, as generated; `target_hz` and `target_vpk` where
  it is going. All four are null until the run has started.
- `sweep` is null, or `{"direction","rate_oct_per_min","halted"}`: `halted` is true once the sweep
  has parked at its edge or a frequency was set by hand (the sweep no longer moves the tone).
- `health` is null until the run has started, then: engine ticks, the worst tick's compute
  against its budget (ms), scheduling-health faults, raw-ring overruns, display blocks the
  analysis dropped, driver warnings so far (each is in the log), the analysis pass's last and
  worst time and the longest gap between the end of one batch and the start of the next (ms;
  null until two batches have run).
- `stall` is null, or `{"stale_s","period_s","message"}` while the service's loop has missed three
  of its periods (and at least 3 s): the run may still be driving, and Abort stays open.
- `stop` is null until a stop begins, then its cause: `operator`, `safe_stop` (nobody held
  control), `shutdown` or `limit` (the derived motion).
- `record` is null until the run's record is open at start, then
  `{"folder","streams","problem"}` as Random's: the run folder's absolute path, `on` or `off`
  (the streams are switched off in the setup, or the disk was under the free-space floor at
  start), and the latest problem writing the record in words, or null. A problem does not end
  the run.
- `end` is null until the run finishes, then
  `{"outcome","message","at_s","at_hz","vpk","ramp_s","trip"}`: `at_s` the station time at which
  the run finished, `at_hz` and `vpk` the tone when the end began, `ramp_s` the controlled stop's
  length on the station's clock, from the input frame the stop was taken at (null for an abort and
  a fault), so the stop began at `at_s - ramp_s`. The messages: `operator_stop` "Stopped by
  <name>: the controlled stop ramped the drive to 0 V."; `safe_stop` "Nobody held control: the
  controlled stop ramped the drive to 0 V." or, at a shutdown, "The service shut down: the
  controlled stop ramped the drive to 0 V."; `operator_abort` "Aborted by <name>."; a trip names
  its channel, axis, value and limit, or the drive sample and the ceiling; a fault is the
  controller's words.

| `outcome` | How the run ended |
|---|---|
| `operator_stop` | `explorer.stop`: the drive ramped to 0 V at the drive slew, then the station stopped |
| `safe_stop` | Nobody held control for `timeout_s`, or the service was shut down; the controlled stop, and `message` says which |
| `limit_stop` | The control row's derived velocity or displacement passed its limit once settled; the controlled stop. `trip` says which |
| `operator_abort` | `explorer.abort`, from any connection |
| `aborted` | A protection tripped; `trip` says which |
| `fault` | The station or the service's loop failed (an underrun, a stalled input, a lost device, a scheduling fault, a loop failure); `message` is the controller's words |
| `arm_refused` | The station or the arm gate refused the start; nothing was driven |

`trip` is null unless `outcome` is `aborted` or `limit_stop`; it has `kind`, `at_hz` and its
kind's facts:

| `kind` | Facts |
|---|---|
| `watchdog` | `channel`, `axis` (`accel`, `velocity` or `displacement`), `value`, `limit`, in the axis's unit (g, in/s, mil) |
| `drive_vpk` | `vpk` (the sample's), `ceiling_vpk` |
| `limit` | `channel`, `axis` (`velocity` or `displacement`), `value`, `limit` |

### Explorer Survey

`explorer.survey` gives the survey line: the response level at each frequency the tone has
visited, `points` sorted by frequency as `[hz, level]` pairs. `level` is the control row's
amplitude at the tone, peak, in its unit (`channel` and `unit` name the row): the rms of the
freshest 100 ms at that frequency over whole cycles, mean removed, times the square root of 2. A
point is taken once the tone has stood for the pipeline, the guard and three cycles, and never
from fewer than three cycles. A visit within `derived.survey.tolerance_hz` of a point replaces
it, and the tone passing through a span (a sweep, or a jump from one point to the next) removes
every older point inside it, so a second pass over a band shows the second pass. `peak` is the
highest point, `{"hz","level"}`, or null. `revision` counts every change; given `since` equal to
it, `points` is null (nothing has changed since). A point carries no time of its own: the steering
answers' and the records' frames say when a change was taken, and the revision moves with every
point. A survey of one point is carried as that point, on its own points or drawn.
`explorer.survey.clear` empties it and answers the new revision; the tone held at the clear is
measured again from the next pass. Refused `wrong_state` outside `running` and `stopping`. The survey lives as long as the run: it is kept after the end until
`explorer.reset`, `explorer.clear` or a new configuration.

### Explorer Records

Every Explorer run that starts leaves a run folder in the service's data folder, laid out in
`docs/run-folder.md` ("What an Explorer Run Saves") and written by the shared run record, which
the analysis side converts into a project file (`hertzforge.io.run_convert`). The folder is
`capture.run_name` under the local data root (`HERTZFORGE_DATA_ROOT`, else `Hertzforge_Data` in
the home folder); a name already taken gets the first free `<name>-<n>`, so no run writes over
another. `run.json`, `table.json` and `setup.json` are written at start, before the station is
opened, or the start is refused `record_unwritable` and nothing is driven. `run.json`'s `grid`
is the rate asked until the station has programmed its own, then the station's (`actual`), and
the streams' `fs_hz` with it. While the run goes, every log entry is appended to `log.jsonl` as
it is made, with the station frame and structured `data`: each steering verb the run took is an
`intent` entry whose `data` is its station time, the generated frame it took effect at, the
input frame it was taken at, what was asked, what was adopted and whether it was clamped. Every
enabled input as read and the drive as the device accepted it are recorded as streams unless
`record.streams` is false. At the end, any way the run ends, the survey line and each acquired
channel's last display spectrum are written as results, and `end.json` holds `end` as
`explorer.state` reports it, the run's state and what it measured; `run.json` then says
`complete`. A service stopped mid-run leaves the folder with `complete` false. The files are
written by the record's own thread: the run's thread hands it entries with sends that never
wait, the streams are taken where the station's blocks pass into buffers made before the run,
and nothing touches a file on the thread that reads the station and plays the output. A write
that fails is a `record` entry in the log and the state's `record.problem`, and the run goes on.

### Explorer Display

Event topics:

- `explorer.state` (text): the state object on every change of `state`, `phase`, `sweep`,
  `stop`, `stall` or `end`.
- `explorer.log` (text): new log entries, as Random's: "records in <path>" and "the station
  programmed <fs> Hz (asked <fs> Hz); the band is <lo> to <hi> Hz" at start, every steering verb
  the run took (`intent`: "steer: <hz> Hz, <vpk> V pk", "sweep <direction> at <rate> oct/min",
  "hold at <hz> Hz" or "stop: <cause>", each part followed by "(asked <value>, clamped)" where it
  was clamped), every driver warning (`note`, "driver: " and the driver's own text), every
  problem writing the record (`record`), "stop began: <cause>" and "ended <outcome>: <message>".
- `explorer.live` (binary, to every connection without an Explorer subscription of its own, 10
  times a second from start until the run has finished, then once more).
- `explorer.data` (binary, to one subscribed connection, in place of `explorer.live`):
  `explorer.live`'s fields but `views`, and `plots`, at `rate_hz` (default 10, so a `time` plot
  comes ten times a second unless the subscription asks for fewer), and at once on subscribe once
  the run has started.

```
{"run_id","source","class","state","phase","t_s","frequency_hz","target_hz","measured_hz",
 "drive_vpk","target_vpk","max_drive_vpk","sweep",
 "channels":[{"index":1,"description":"Control","role":"control","rms":0.1413,"unit":"g"},
             {"index":3,"description":"Fixture","role":"monitor","rms":0.0706,"unit":"g"}],
 "control":{"index":1,"velocity_ips":0.04916,"displacement_mil":0.0313,"settling":false},
 "pipeline_frames":1063,"peak":{"hz":250,"level":0.1999},"blocks_dropped":0,
 "pass_ms":{"last":1.9,"worst":3.4},
 "views":[survey, run readings]}
```

- `t_s`, `frequency_hz`, `target_hz`, `drive_vpk`, `target_vpk`, `max_drive_vpk`, `sweep`,
  `pipeline_frames`: as in the state.
- `measured_hz` is the frequency measured on the control row by its zero crossings over 0.5 s
  (and at least 12 crossings), null until a tone at a level above 0 V has held for a pass and the
  pipeline's worth of input has passed. It is a cross-check of the tone the service commands, not
  of the station's rate: it counts samples at the rate the service believes.
- `channels` lists every acquired channel in table order with its rms over the last analysis
  pass, DC included, in its unit.
- `control` is the control row's derived velocity (in/s) and displacement (mil), each the peak
  over the last analysis pass, with `settling` true while the integrators' start-up transient
  lasts (five time constants of their 5 Hz high-pass); both are null when the control row's unit
  is not `g`.
- `peak` is the survey's highest point, or null; `blocks_dropped` and `pass_ms` as in `health`.
- `views` are **Survey** (`spectrum`, x `Frequency` Hz log, y `Level` in the control row's unit,
  log: `Survey` (`measured`), the survey line drawn on 256 log-spaced points across the band,
  nothing outside the span visited; `link` `frequency`) and **Run** (a `reading`, in this order:
  `Frequency` and `Target` Hz, `Measured` Hz (`none` while there is no measurement), `Drive` and
  `Target drive` V, `Ceiling` V, `Sweep` (`up at 30 oct/min` while it moves the tone, `parked`
  once at its edge, or `none`), `Control rms` in the control row's unit, `Velocity` in/s and
  `Displacement` mil (`none` when the control row's unit is not `g`), `Peak` Hz (`none` before the
  first point), `Pipeline` frames; all `info`). The shell marks the tone and the peak on the survey
  from `frequency_hz` and `peak`.

`explorer.subscribe` takes Random's form: `{"rate_hz","plots":[plot...]}` (`rate_hz` 0.5 to 10,
default 10; at most 64 plots, each `id` unique, 1 to 64 characters) or `{"default":true}` (back to
`explorer.live`); a key a quantity does not take is `bad_params` naming it. A plot is
`{"id","quantity","channels","points"}`: `channels` applies to `spectrum` (a list of indexes, or
`"all"` or `"control"`; default `"all"`), `points` to `survey` (null: the survey's own points;
64 to 8192: drawn on that many log-spaced points across the band) and to `time` (64 to 8192,
default 1024). The result is `{"default","rate_hz","plots","quantities"}`, every default filled
in: a `survey` plot states `points` (null for its own points), a `spectrum` plot `channels`, a
`time` plot `points`, and a `readings` plot neither. In `explorer.data` each plot is
`{"id","quantity","channels","first_frame","views"}`: `channels` the indexes `spectrum` resolved
to, in table order, and null for the rest;
`first_frame` the input frame of a `time` plot's first sample, and null for the rest.

| `quantity` | Views | What they show |
|---|---|---|
| `survey` | one `spectrum` | The live frame's **Survey**, on the survey's own points (joined by straight lines in log frequency) or drawn at `points` |
| `spectrum` | one `spectrum` per resolved channel | Titled `Channel <index>: <description>` (the role when there is no description), x `Frequency` Hz linear from 0 to half the rate at the display line spacing, y `Amplitude` in the channel's unit, linear: `Channel <index>` (`measured`), the latest display window's magnitude (Hann, amplitude-corrected, peak); `link` `frequency` |
| `time` | one `spectrum` | **Time**: the control row's most recent `points` samples, x `Time` s of station time (the input frame over the rate), y `Amplitude` in its unit, linear: `Channel <index>` (`measured`) |
| `readings` | one `reading` | The live frame's **Run** reading |

A plot is empty (`views` empty) until there is something to draw: `survey` until its first
point, the others until the first analysis pass.

## Recorder

The Recorder records the inputs and drives nothing. A run opens the station with no outputs,
acquires every enabled input row of the channel table, shows each row's level, time history and
spectrum live, and saves recordings of those rows into its run folder: from the run's first frame,
when the operator asks, or each time a row crosses a level. Drive rows of the table are not opened
and not recorded, so the one table serves the drive modules and the Recorder alike. Monitoring
saves no samples; only a recording does. The run is one test like any other: a Recorder run,
an Explorer run, a TWR run, a Sine run, an SRS shock run, a classical shock run, a Random run, a
MIMO run and a `sim.start` run exclude each other, each refused `run_active` while another is
live. **Any connected screen may stop or abort** (`recorder.stop`, `recorder.abort`);
`recorder.configure` (other than a dry run), `start`, `arm`, `disarm`, `record`, `cut`, `reset`
and `clear` need control; everything that only reads is open to every connection. While the
service is shutting down, `recorder.start`, `arm` and `record` are refused `shutting_down`;
`recorder.disarm`, `cut`, `stop` and `abort` are not.

| Method | Params | From | Result |
|---|---|---|---|
| `recorder.configure` | `{"config","dry_run"}` (`dry_run` optional) | any but live | `{"state","derived","warnings"}`; a dry run `{"errors","warnings","derived"}` |
| `recorder.config` | `{}` | any | `{"run_id","config","derived"}` |
| `recorder.start` | `{}` | `configured` | state, once acquisition runs |
| `recorder.arm` | `{}` | `running` (`monitoring`); `manual` and `level` modes | `{"pre_frames","held_frames"}` |
| `recorder.disarm` | `{}` | `running` (`armed`) | state |
| `recorder.record` | `{}` | `running` (`monitoring` or `armed`); `manual` mode | `{"name","station_frame","pre_frames"}` |
| `recorder.cut` | `{}` | `running` (`recording`) | `{"name","last_station_frame"}` |
| `recorder.stop` | `{}` | any | state |
| `recorder.abort` | `{}` | any | state |
| `recorder.recordings` | `{"since"}` (optional, default 0) | any | `{"recordings","next"}` |
| `recorder.marks` | `{"since"}` (optional, default 0) | any | `{"marks","next"}` |
| `recorder.reset` | `{}` | any but live | state |
| `recorder.clear` | `{}` | any but live | state |
| `recorder.state` | `{}` | any | state |
| `recorder.log` | `{"since"}` (optional, default 0) | any | `{"entries","next"}` |
| `recorder.subscribe` | see "Recorder Display" | any | as there |

"state" in the Result column is the `recorder.state` object below. A run is live while it is
starting, running or stopping. `recorder.configure` replaces a run that is not live;
`recorder.reset` returns an ended run to `configured` with its configuration on the present table
and station, the recordings list, the marks and the log empty (the files stay on disk);
`recorder.clear` forgets it (`unconfigured`). While a run is live both are refused `run_active`
(naming that run), and so is `recorder.reset` while another module's run is live, as a configure
would be. `recorder.start` on a run already started is refused `wrong_state`. A verb used in a
trigger mode it does not apply to is refused `wrong_mode` with `detail.mode`; in a phase it does
not apply to, `wrong_state` with `detail.state` and `detail.phase`.

### Recorder Configuration

```
{"title":"Fixture A, run 4",
 "acquisition":{"fs_hz":12800},
 "trigger":{"mode":"level","row":1,"level":0.25,"slope":"rising","hysteresis":null,
            "pre_s":0.5,"record_s":1.0,"count":5,"holdoff_s":0},
 "analysis":{"block":2048,"window":"hann","overlap_pct":null,"averaging":"linear",
             "averages":8,"decimation":1,"frf":{"reference":2,"response":4}},
 "capture":{"run_name":"recorder","keep_free_gb":1},
 "metadata":[{"key":"run_number","label":"Run Number","value":"4"}]}
```

| Field | Default | Meaning |
|---|---|---|
| `title` | empty | The run's name |
| `acquisition.fs_hz` | 12800 | The rate asked of the station, 1000 to 204800 Hz. A station may program another rate; the run adopts it, records at it, derives every per-frame number from it and reports both (`grid`) |
| `trigger.mode` | `manual` | What starts a recording: `start` (the run's first frame), `manual` (`recorder.record`) or `level` (a row crossing `level`) |
| `trigger.row` | (required in `level` mode) | The `index` of the row the level is read on: a recorded row (`row_not_recorded`) |
| `trigger.level` | (required in `level` mode) | The level, in the row's unit, within the row's `range_max` either side of zero (`out_of_range`) |
| `trigger.slope` | `rising` | `rising` or `falling` |
| `trigger.hysteresis` | null: 1% of the row's `range_max` | The band, in the row's unit, above 0 and at most `range_max`: before the detector arms the signal must have been below `level - hysteresis` (rising) or above `level + hysteresis` (falling), so a signal hovering at the level fires at most once |
| `trigger.pre_s` | 1.0 | The pre-window, seconds, 0 to 10: the frames before the trigger that a `manual` or `level` recording begins with |
| `trigger.record_s` | null | Each recording's length after its trigger, seconds, above 0. Null: until `recorder.cut` or the run's end. Required in `level` mode |
| `trigger.count` | 1 | `level` mode: how many recordings the trigger saves after `recorder.arm`, at least 1, or null for until the run ends |
| `trigger.holdoff_s` | 0 | `level` mode: the least station time, seconds, from one trigger to the next, at least 0 |
| `analysis.block` | 2048 | The analysis block, frames at the analysis rate, a power of two from 256 to 65536 |
| `analysis.window` | `hann` | `hann`, `flattop`, `rectangular` or `blackman_harris` |
| `analysis.overlap_pct` | null: the window's own | 0 to 95. The window's own: Hann 50, flat top 75, rectangular 0, Blackman-Harris 66.7 |
| `analysis.averaging` | `linear` | `linear`, `exponential` or `peak_hold` |
| `analysis.averages` | 8 | At least 1: blocks in a linear average, the time constant in blocks of an exponential one |
| `analysis.decimation` | 1 | A power of two from 1 to 256. The analysis rate is the station's rate over it; recordings are always at the station's rate |
| `analysis.frf` | null | `{"reference","response"}`, two different recorded rows by `index` (`frf_same_row`, `row_not_recorded`), for the live FRF; or null |
| `capture.run_name` | `recorder` | The run folder's name under the local data root (see "Recorder Records"), a plain name with no path in it |
| `capture.keep_free_gb` | 1 | The free space the run leaves on the data volume, GiB (1 GiB is 1 073 741 824 bytes), at least 1 |
| `metadata` | the default fields | Random's: `{"key","label","value"}`, `label` the key when absent |

The recorded rows are every enabled input row (`control`, `monitor` or `reference`), in table
order, and each recording holds one column per recorded row in that order. Enabled `drive` rows
are not opened and not recorded: configure answers the warning `drive_channel_present` naming
them, and nothing else changes. The table must have at least one enabled input row
(`no_input_channel`). A row's `abort` limits do not stop a Recorder run: a row passing one is
marked and recording goes on (see "Recorder Marks"). A configuration whose shape is wrong is
refused `bad_params` naming the field in `detail.param` as the request's own path
(`config.trigger.level`): a missing required field (in `level` mode `trigger.row`,
`trigger.level` and `trigger.record_s`), a value of the wrong type or not one of its names, or a
key the configuration does not have. Its content errors are refused `config_invalid` with the
items, located by `field`, the path inside the configuration (`trigger.pre_s`, with no `config.`
before it). `dry_run: true` checks without replacing anything and needs no control.

Item reasons (Recorder): errors `out_of_range`, `not_power_of_two` (`analysis.block`,
`analysis.decimation`), `row_not_recorded` (the trigger row or an FRF row is not an enabled input
row), `frf_same_row`, `no_input_channel`; warnings `drive_channel_present`.

**Result.** `state` is the `recorder.state` object (`configured`). `derived`, here for an NI
station with four recorded rows at 12 800 Hz and the configuration above:

```
{"grid":{"fs_hz":12800,"block_frames":256,"batch_frames":512,"group_delay_frames":38.44,
         "actual":false},
 "outputs":"none","rows":[1,2,3,4],
 "analysis":{"fs_hz":12800,"df_hz":6.25,"lines":800,"span_hz":5000,"block_s":0.16,
             "overlap_pct":50,"averages_s":0.72},
 "trigger":{"pre_frames":6400,"record_frames":12800,"holdoff_frames":0},
 "disk":{"bytes_per_s":204800,"floor_bytes":1073741824,"free_bytes":107374182400,
         "seconds_above_floor":519045}}
```

- `grid`: the requested rate, and the block, the batch (two blocks: the most the service reads
  between two verbs) and the input group delay the selected station works in at it; `actual` is
  true once the run has started and all of them are the station's. The block is the power of two
  nearest 20 ms of the rate. The group delay is in frames as the station states it, to 0.01: on an
  NI input path 38.4 + 3.2e-6 times the rate (38.44 at 12 800 Hz), on the simulator 0.
- `outputs` is always `"none"`: a Recorder run opens no output.
- `rows`: the recorded rows' `index`es, in column order.
- `analysis`: the analysis rate (the station's rate over `decimation`), the line spacing, the
  alias-free lines (the block over 2.56) and span (the analysis rate over 2.56), a block's
  length in seconds, the overlap in force, and how long after start the first full average is
  complete: the block plus `averages - 1` hops (a hop is the block less the overlap), at the
  analysis rate.
- `trigger`: the pre-window, a recording's length after its trigger (null until cut) and the
  holdoff, in frames at the rate, each `round(seconds x fs_hz)`.
- `disk`: the bytes a second a recording writes (4 bytes a row a frame), the free-space floor
  (the larger of `keep_free_gb` and twice one recording's bytes, the pre-window included, when
  `record_s` is set), the data volume's free bytes now, and the whole seconds of recording that
  fit above the floor now. `free_bytes` and `seconds_above_floor` are null when the free space
  cannot be read.

### Running a Recorder Test

- **`recorder.start`** opens the station at `acquisition.fs_hz` with no outputs, adopts the rate
  it programs, passes the arm gate as a capture-only run, makes the run folder and starts
  acquisition (`starting`, `opening`, while the station opens). It answers once acquisition runs:
  `running`, `monitoring`, or in `start` mode `recording`, the first recording beginning at the
  run's first frame (station frame 0, no pre-window). Refusals, in the order they are checked:
  `shutting_down`, `not_controller`, `not_configured`, `run_finished` (reset first), `wrong_state`
  (already started), `run_active` (another module's run is live; it names that run, so a live
  Explorer run refuses a Recorder start and a live Recorder run refuses an Explorer start, each
  naming the other), `table_invalid`, `config_invalid` (the table, as it is now, no longer carries
  the run: the same items as configure), `not_enough_space` (the free space is under the floor;
  `detail` `free_bytes` and `floor_bytes`), then, once the station is being opened,
  `devices_absent`, `hardware_busy` and `arm_refused` (the station or the arm gate refused, the
  message the controller's own; or, once the station is up, the run folder, `run.json`,
  `table.json`, `setup.json` or `streams/` could not be written, `detail` `path` and `reason`).
  No refusal before the station is opened makes a folder. A refusal once the station is being
  opened ends the run `arm_refused`, and `recorder.reset` comes before another start.
- **`recorder.arm`** (`manual` and `level` modes, from `monitoring`) starts filling the
  pre-window with the frames acquired from the next batch on, and in `level` mode arms the
  detector. It answers `pre_frames`, the pre-window asked, and `held_frames`, the frames it holds
  now. Refused `wrong_mode` in `start` mode and `wrong_state` outside `monitoring`.
- **`recorder.disarm`** returns an armed run to `monitoring` and drops the pre-window. Refused
  `wrong_mode` in `start` mode and `wrong_state` outside `armed`.
- **`recorder.record`** (`manual` mode, from `monitoring` or `armed`) begins a recording. Its
  trigger is the first frame of the next batch: `station_frame` in the answer. Armed, the
  recording begins with the pre-window held, at most `pre_s`; a pre-window that has not filled is
  stated in the recording (`pre_frames` under `pre_frames_asked`). Not armed, it has no
  pre-window. Refused `wrong_mode` outside `manual` mode and `wrong_state` in a recording.
- **`recorder.cut`** ends the recording in progress with the batch the request is taken in;
  `last_station_frame` is its last frame. Refused `wrong_state` outside a recording.
- **A level trigger** fires on the first sample of `trigger.row` at or over `level` (rising) or
  at or under it (falling) once the detector has armed; that sample is the recording's trigger,
  the pre-window before it. The recording holds exactly `round(record_s x fs_hz)` frames from the
  trigger on, plus its pre-window. After it the trigger re-arms by itself until `count`
  recordings have been saved since `recorder.arm`, then the run returns to `monitoring`. A
  re-armed trigger fires no sooner than `holdoff_s` of station time after the last trigger, and
  not before a full pre-window of frames acquired after the last recording ended, so no frame is
  in two recordings. The first fire after `recorder.arm` may come before the pre-window has
  filled; the recording then states the pre-window it holds.
- **A recording ends** at its length (`record_s`), at `recorder.cut` (`start` and `manual`
  modes), or with the run. When a `start` or `manual` recording ends the run returns to
  `monitoring`, not armed.
- **`recorder.stop`** and **`recorder.abort`** end acquisition at the next batch; every recording
  is finalized with every frame acquired, and nothing is discarded (`stopping`, `closing`, while
  the files are finished). Only the outcome differs: `operator_stop` or `operator_abort`. Each
  answers at once, with the state as it stands.
- **The disk.** Recordings are written by the run's own record thread, never by the thread that
  reads the station; the reading thread hands each batch to it through a queue that holds 2 s of
  frames. A write that fails (the disk refuses it, an I/O error, or the free space falls under the
  floor) ends the run `write_failed` with the fault's own words; the recording is finalized with
  what reached the disk and names the fault. A disk so far behind that a batch cannot be queued
  ends the recording there, with the gap recorded, and the run `write_behind`. The backlog is in
  the state (`recording.backlog_s`).
- **The station.** An input buffer overflow, a lost device, an input stall or any other failure
  of the station or of the service's loop ends the run `fault` with the controller's words, every
  recording finalized.
- **When nobody holds control** the run goes on unchanged, recordings included; after
  `timeout_s` it ends `safe_stop` with every recording finalized, as a stop does. While the
  service shuts down it ends the same way.

**Time is the station's.** `t_s` in the state and the live frames, `end.at_s`, the answers' and
the recordings' station frames, the marks' and the time plots' `first_frame` count the station's
own input frames from the run's first frame (station frame 0) at the rate it programmed, never the
service's clock. A station that states an input group delay of `g` frames reads at station frame
`n` the signal that reached its input `g` frames earlier, so an event at the input at station time
`t` is recorded at frame `t x fs_hz + g`. The safe stop's countdown (`orphan_s`) is the only
number timed by the service's clock.

### Recorder Run State

```
{"run_id":"recorder-0002","state":"running","phase":"recording","title":"Fixture A, run 4",
 "source":"HARDWARE","station":{"kind":"ni","class":"ni_real","devices":[...]},
 "grid":{"requested_fs_hz":12800,"fs_hz":12800,"block_frames":256,"batch_frames":512,
         "group_delay_frames":38.44,"actual":true},"clock":"shared",
 "outputs":"none","rows":[1,2,3,4],"mode":"level","t_s":11.36,"station_frames":145408,
 "trigger":{"armed":false,"pre_frames":6400,"held_frames":0,"fires":2,
            "last_station_frame":134422,"saved":1,"left":3,"holdoff_left_s":0},
 "recording":{"name":"rec_0002","trigger":"level","first_station_frame":128022,
              "trigger_station_frame":134422,"pre_frames":6400,"pre_frames_asked":6400,
              "frames_fed":17386,"frames_on_disk":16384,"target_frames":19200,
              "seconds":0.858,"length_s":1,"backlog_s":0.078},
 "recordings":1,"marks":0,
 "disk":{"free_bytes":107373363200,"floor_bytes":1073741824,"seconds_above_floor":519041,
         "fault":null},
 "health":{"batches":284,"blocks_dropped":0,"driver_warnings":0,"station_errors":0,
           "pass_ms":{"last":1.2,"worst":2.8},"batch_gap_ms":3.9},
 "stop":null,"end":null,
 "record":{"folder":"/home/operator/Hertzforge_Data/recorder","problem":null}}
```

| `state` | `phase` | Meaning |
|---|---|---|
| `unconfigured`, `configured` | `idle` | No configuration; configured, nothing open |
| `starting` | `opening` | `recorder.start`: the station is opening; nothing is recorded |
| `running` | `monitoring` | Acquiring and showing; nothing is being saved |
| `running` | `armed` | The pre-window is filling; in `level` mode the detector waits (a holdoff included) |
| `running` | `recording` | A recording is being saved |
| `stopping` | `closing` | Acquisition has ended; the record thread is finishing the files |
| `finished` | `stopped`, `aborted`, `faulted` | Ended; `end` says how |

- A finished run's phase is `stopped` after `operator_stop` or `safe_stop`, `aborted` after
  `operator_abort`, and `faulted` after `write_failed`, `write_behind`, `fault` or `arm_refused`.
- `grid`: as in `derived`, with `requested_fs_hz` the rate asked and `fs_hz` the rate the station
  programmed once started (the request until then). `clock` as Random's.
- `outputs` and `rows` as in `derived`; `mode` is `trigger.mode`.
- `t_s`: station time since start, and `station_frames` the frames acquired; both null before.
- `trigger` is null in `start` mode, else: `armed` (the detector or the manual pre-window is
  armed), the pre-window asked and the frames it holds now, the triggers since start (`fires`),
  the station frame of the last (null before one), the recordings `saved` since the last
  `recorder.arm`, the recordings of `count` not yet begun (`left`; null when `count` is null or
  in `manual` mode), and in `level` mode the holdoff left, seconds of station time (0 when none).
- `recording` is null, or the recording in progress: its name, its trigger mode, the station frame
  of its first frame and of its trigger, the pre-window held and asked, the frames handed to the
  record thread (`frames_fed`) and those on disk, `target_frames` (the pre-window plus
  `record_frames`, null until cut), `seconds` recorded after the trigger, `length_s`
  (`record_s`, null until cut) and `backlog_s`, the frames fed and not yet on disk, in seconds.
- `recordings` counts the recordings saved; `marks` counts the limit marks (see "Recorder Marks").
- `disk`: the free bytes, the floor and the seconds above it, read once a second (null when the
  free space cannot be read), and `fault`, null or the record thread's fault in its own words.
- `health` is null until the run has started, then: batches read, display blocks the analysis
  dropped, driver warnings so far (each is in the log), station errors, the analysis pass's last
  and worst time and the longest gap between the end of one batch and the start of the next (ms;
  null until two batches have run).
- `stop` is null until a stop begins, then its cause: `operator`, `safe_stop` (nobody held
  control) or `shutdown`.
- `end` is null until the run finishes, then `{"outcome","message","at_s","recordings"}`:
  `at_s` the station time at which acquisition ended, `recordings` the recordings saved. The
  messages: `operator_stop` "Stopped by <name>: every recording is saved."; `safe_stop` "Nobody
  held control: the run stopped and every recording is saved." or, at a shutdown, "The service
  shut down: the run stopped and every recording is saved."; `operator_abort` "Aborted by
  <name>: every recording is saved."; `write_failed` the fault's words and the recording that
  holds what was written; `write_behind` how far behind the disk was; a fault is the controller's
  words.
- `record` is null until the run has started, then the run's record: `folder`, the run folder
  (see "Recorder Records"), and `problem`, null or the latest problem writing the run folder's own
  files in words (a log line it could not write, say; each is also a `record` entry in the log).
  A recording that cannot be written is not a problem here: it ends the run `write_failed`.

| `outcome` | How the run ended |
|---|---|
| `operator_stop` | `recorder.stop` |
| `safe_stop` | Nobody held control for `timeout_s`, or the service was shut down; `message` says which |
| `operator_abort` | `recorder.abort`, from any connection |
| `write_failed` | A write failed or the free space fell under the floor; `message` is the fault's words |
| `write_behind` | The disk fell so far behind that a batch could not be queued |
| `fault` | The station or the service's loop failed (an input buffer overflow, a lost device, an input stall); `message` is the controller's words |
| `arm_refused` | The station or the arm gate refused the start; nothing was recorded |

### Recorder Recordings

`recorder.recordings` lists the run's recordings from `since` (a position in the list, from 0),
the one in progress last with `complete` false, and `next`, the position after the last:

```
{"name":"rec_0001","folder":"streams/rec_0001","trigger":"level",
 "requested_fs_hz":12800,"fs_hz":12800,"group_delay_frames":38.44,"rows":[1,2,3,4],
 "first_station_frame":102422,"trigger_station_frame":108822,"pre_frames":6400,
 "pre_frames_asked":6400,"frames":19200,"seconds":1.5,"bytes":307200,
 "gap_free":true,"gaps":[],"marks":0,"complete":true,"fault":null}
```

`folder` is relative to the run folder. `frames` and `bytes` are the file's, `seconds` its frames
over `fs_hz`; `gaps` lists every place frames are missing, `{"at_frame","dropped_frames"}` (the
file frame the missing frames would have started at, and how many); `marks` counts the limit marks
inside it. The other fields are its `stream.json`'s (see "Recorder Records").

### Recorder Marks

A row's `abort` limits are marks, not stops: the Recorder drives nothing, so ending acquisition
would protect nothing and lose the event. `abort.accel_g` is checked on every sample of a row in
g, and `velocity_ips` and `displacement_mil` on a row's derived velocity and displacement (rows in
g). A row passing a limit is marked at the station frame of its first sample past it; the same
row and limit is marked again only after it has stayed within the limit for one second of station
time. Each row also has an overload indicator: it latches the first time a sample's magnitude
reaches 0.9 of the row's `range_max`, holds for the run, and is marked once. The station's own
overload words, where it says any, are in the log.

`recorder.marks` gives them from `since` (a position, from 0) with `next`:
`{"kind":"limit","row","axis","value","limit","station_frame","recording"}` (`axis` `accel`,
`velocity` or `displacement`, in the axis's unit: g, in/s, mil) or
`{"kind":"overload","row","value","level","station_frame","recording"}` (`level` 0.9 of the
row's `range_max`); `recording` names the recording the mark fell in, or null.

### Recorder Records

Each run writes its folder `capture.run_name` under the local data root (`HERTZFORGE_DATA_ROOT`,
else `Hertzforge_Data` in the home folder); a name already taken gets the first free `<name>-<n>`,
so no run writes over another. The state names it from start on (`record.folder`), and the log
names it at start ("records in <path>"). The wire offers no other place. The folder is a run
folder (`docs/run-folder.md`, schema `hertzforge-run/1`), which the analysis side converts into a
project (`hertzforge.io.run_convert`):

```
<run folder>/run.json
             table.json
             setup.json
             log.jsonl
             end.json
             streams/rec_0001/telemetry.f32
                              stream.json
                              capture.json
```

- `run.json`, written at start and rewritten as each recording begins and at the end: the
  manifest, `test` `recorder`, with the run's identity (`run_uid`, unique across restarts, and
  `run_id`), `title`, `operator`, `article`, `notes`, `specification`, `metadata`, `source`, `sim`
  on the simulator, `station` (its devices with their serial numbers), `grid` (the rate asked and
  programmed, `block_frames`, `batch_frames`, `group_delay_frames`, the analysis rate, line
  spacing and lines), `build`, `start` (`unix`, `utc_offset_s`, `station_frame` 0), `streams`
  (each recording begun, `kind` `recording`) and `complete`.
- `table.json`: every row of the channel table the run started with, in the spelling of
  `channels.get`, with its `revision` and `sha256`.
- `setup.json`: `config` (the configuration as sent), `resolved` (the acquisition at the
  programmed rate, `outputs` `"none"`, `rows` (each recorded row's `index`, `description`,
  `role`, `unit` and `range_max`, in column order), the trigger with its hysteresis and its
  frames, the analysis and the capture with its floor), `derived` (as configure's, at the
  programmed rate, `actual` true) and `overload` (`fraction`, 0.9, and `definition`, the
  indicator's definition in words).
- `log.jsonl`: a line for every log entry, `{"seq","at_unix","frame","kind","text"}` as
  `recorder.log` gives it with `frame`, the station frame when the run's thread passed it to
  the record, ten times a second (null before the first batch); a mark's line (`mark` or `overload`) has the mark's own `station_frame` as its
  `frame` and the mark, as `recorder.marks` gives it, as its `data`.
- `end.json`, written when the run finishes: `run_uid`, `run_id`, `end` (as in the state),
  `at_unix`, `station_frames` (the frames acquired), `rows`, each recorded row's statistics over
  the whole run (`index`, `mean`, `rms`, `peak`, `crest` and `kurtosis`, kurtosis 3 for a Gaussian
  signal), and `streams`, each recording's `frames`, `drops`, `gaps`, `fault` and `complete`. The
  finished state is published once it is written.
- `streams/rec_NNNN/`, numbered from `rec_0001` in the order they began:
  - `telemetry.f32`: no header; little-endian 32-bit floats, one frame after another, each frame
    one value per recorded row in `rows` order, in the row's unit, at `fs_hz`. The file's size is
    the authority on its length: a reader counts its frames as the size over 4 times the rows.
  - `stream.json`: the recording's facts, written whole before any frame and again when the
    recording ends.
  - `capture.json`: the writer's own record: the channel table at capture time, the rate
    (`config.sample_rate_hz`, the programmed rate), `frames_written`, `chunks_dropped`, `flushes`,
    `free_space_floor_bytes`, `complete` and `fault`.

| `stream.json` field | Meaning |
|---|---|
| `kind` | `recording` |
| `channels` | The recorded rows, `{"index","unit","description"}`, in column order |
| `complete` | False until the recording is finalized: a recording found with `complete` false was cut short by a crash, and its file holds what reached the disk |
| `name`, `trigger` | `rec_NNNN`; `start`, `manual` or `level` |
| `requested_fs_hz`, `fs_hz` | The rate asked and the rate the station programmed; the file is at `fs_hz` |
| `group_delay_frames` | The station's stated input group delay, frames |
| `rows`, `units` | The recorded rows' `index`es and units, in column order |
| `first_station_frame` | The station frame of the file's first frame |
| `trigger_station_frame` | The station frame of the trigger, file frame `pre_frames`; t = 0 |
| `pre_frames`, `pre_frames_asked` | The pre-window held and asked; held less than asked when it had not filled |
| `frames` | The frames written, as the writer counted them (0 until finalized) |
| `gaps` | `[{"at_frame","dropped_frames"}]`, empty when gap-free |
| `marks` | The marks inside the recording, as `recorder.marks` gives them |
| `fault` | Null, or the write fault's words |
| `writer` | Once finalized: the writer's `ring_capacity`, `chunks_written`, `chunks_dropped`, `flushes`, `free_space_at_start_bytes` and `free_space_floor_bytes` |

A frame's time from the trigger is `(file frame - pre_frames) / fs_hz` seconds, and its station
frame is `first_station_frame` plus its file frame plus the frames missing before it. A reader
ignores fields it does not know. The run folder's own files are written by the run's record
thread and the recordings by the Recorder's record thread, never by the thread that reads the
station; the recordings are flushed and synced every 5 s and the log every 5 s, so a crash loses
at most the last 5 s.

### The Recorder on the Simulator

The simulator's plant moves only when it is driven, and a Recorder run drives nothing. So on the
simulator a Recorder run's inputs carry a stated source, added to the plant's noise: the k-th
recorded row, in table order, carries row k of the table below, and a fifth row and after repeat
the four in order. The source repeats every 2.000 s from the run's first frame; each tone starts
at phase 0 and makes whole cycles in that period. Levels are peak, in each row's unit: the
simulator works in the row's unit, so a row's sensitivity does not scale it. The shell names the
source whenever a Recorder run is on the simulator. The wire offers no way to change it.

| Row | Signal |
|---|---|
| 1 | Markers: a half-sine of 0.5 peak lasting 10 ms, beginning 0.5 s into every 2.000 s period; 0 between |
| 2 | 250 Hz, 0.2 peak |
| 3 | 433 Hz, 0.1 peak |
| 4 | 1000 Hz, 0.15 peak |

At station time `t = n / fs_hz` (frame `n` over the rate the station programmed), row 1 is
`0.5 sin(pi (t - t_k) / 0.010)` for `t_k <= t < t_k + 0.010`, `t_k = 0.5 + 2k` s, and 0
otherwise; rows 2 to 4 are `A sin(2 pi f t)`. At 12 800 Hz a marker begins at station frame
6 400 + 25 600 k and lasts 128 frames, and rises through 0.25 at 1.667 ms (21.33 frames) after
it begins. At 12 800 Hz the 250 and 1000 Hz tones make whole cycles in every 2048 frames and
the 433 Hz tone does not, so a frame missing from a recording moves its phase.

### Recorder Display

Event topics:

- `recorder.state` (text): the state object on every change of `state`, `phase`, `stop` or `end`.
- `recorder.log` (text, to every connection): new log entries, as Random's, of the kinds
  `state`, `station`, `recording` (a recording began or was saved), `mark`, `overload` and
  `note`: when the station programs another rate than asked, "sample rate coerced: requested
  <fs> Hz, device programmed <fs> Hz"; then "the station programmed <fs> Hz (asked <fs> Hz)",
  "records in <path>" (the run folder's absolute path) and "outputs: none" at start (naming any
  drive row not opened), every trigger and recording ("rec_0002 began at station frame <n>",
  "rec_0002 saved: <frames> frames, <seconds> s, no gaps"), every mark and overload, every driver
  warning (`note`, "driver: " and the driver's own text), "stop began: <cause>" and "ended
  <outcome>: <message>".
- `recorder.live` (binary, to every connection without a Recorder subscription of its own, 10
  times a second from start until the run has finished, then once more).
- `recorder.data` (binary, to one subscribed connection, in place of `recorder.live`):
  `recorder.live`'s fields but `views`, and `plots`, at `rate_hz` (default 10), and at once on
  subscribe once the run has started.

```
{"run_id","source","class","state","phase","t_s","station_frames",
 "grid":{"requested_fs_hz":12800,"fs_hz":12800,"group_delay_frames":38.44},
 "rows":[{"index":1,"description":"Table","role":"control","unit":"g","rms":0.1414,
          "peak":0.2004,"overload":false,"marks":0}, ...],
 "trigger","recording","recordings","marks","disk","health",
 "views":[run readings]}
```

- `t_s`, `station_frames`, `trigger`, `recording`, `recordings`, `marks`, `disk` and `health`: as
  in the state.
- `rows` lists every recorded row in table order: its rms over the last second of station time
  and its largest magnitude since start, in its unit (DC included), whether its overload
  indicator has latched, and its limit marks so far.
- `views` is **Run** (a `reading`, in this order: `Rate` Hz (the rate the station programmed),
  `Asked` Hz, `Group delay` frames, `Recording` (the name, or `none`), `Recorded` s after the
  trigger, `Backlog` s, `Saved`, `Fires`, `Marks`, `Free above floor` s; all `info`).

`recorder.subscribe` takes Random's form: `{"rate_hz","plots":[plot...]}` (`rate_hz` 0.5 to 10,
default 10; at most 64 plots, each `id` unique, 1 to 64 characters) or `{"default":true}` (back to
`recorder.live`); a key a quantity does not take is `bad_params` naming it. A plot is
`{"id","quantity","channels","points","seconds"}`: `channels` applies to `time` and `spectrum` (a
list of recorded rows' indexes, or `"all"`; default `"all"`), `points` and `seconds` to `time`
(`points` 64 to 8192, default 1024; `seconds` above 0 and at most 2, default 2). The result is
`{"default","rate_hz","plots","quantities"}`, every default filled in: each plot comes back with
the keys its quantity takes (`time` `channels`, `points` and `seconds`; `spectrum` `channels`;
`frf` and `readings` none). In `recorder.data` each plot
is `{"id","quantity","channels","first_frame","views"}`: `channels` the indexes it resolved to, in
table order, and null for the rest; `first_frame` the station frame of a `time` plot's first
sample, and null for the rest.

| `quantity` | Views | What they show |
|---|---|---|
| `time` | one `spectrum` | **Time**: each resolved row's last `seconds` of samples, x `Time` s of station time, y `Amplitude` in the rows' unit, linear: `Channel <index>` (`measured`), an envelope at `points` (`y` the largest and `y_min` the smallest sample of each interval) |
| `spectrum` | one `spectrum` per resolved row | Titled `Channel <index>: <description>` (the role when there is no description), x `Frequency` Hz linear from 0 to the analysis span at the line spacing, y `PSD` in the row's unit squared per Hz, log: `Channel <index>` (`measured`), the average so far; `link` `frequency` |
| `frf` | three `spectrum` | For the configured pair, each x `Frequency` Hz linear over the analysis span, `link` `frequency`: **FRF** (y `Magnitude`, the response row's unit over the reference row's, log: `H1`), **Phase** (y `Phase` degrees, linear: `H1`) and **Coherence** (y `Coherence`, 0 to 1, linear) |
| `readings` | one `reading` | The live frame's **Run** reading |

A plot is empty (`views` empty) until there is something to draw: `time` until the first batch,
`spectrum` and `frf` until the first average, `frf` without a pair always.

## Control Laws

The control laws an operator can choose, for single-output and MIMO Random, for classical and
SRS shock, for Sine and for TWR alike: the laws built into the program, and law files imported into the local data
folder.

| Method | Params | Result |
|---|---|---|
| `laws.list` | `{"world"}` (optional: `single`, `mimo`, `shock_classical`, `shock_srs`, `sine`, `twr`, `shock_mimo`, or absent for every world) | `{"laws":[entry...],"problems":[{"file","reason"}]}` |
| `laws.import` | `{"path"}` | `{"law":entry}` |
| `laws.check` | `{"id"}` or `{"path"}`, and `{"params"}` and `{"mixed"}` (optional; `mixed` see "Mixed Mode Laws") | `{"law":entry,"report":{"passed","recorded","tool_version","cases":[case...]}}` |

```
{"id":"pinv_closed","world":"mimo","builtin":true,"name":"pseudo-inverse, closed loop",
 "version":"1.0.0","author":"...","summary":"...","sha256":null,
 "check":{"status":"built_in","date":null,"tool_version":null,"cases":[],"hardware":true,
          "message":"Built in: cleared for hardware."},
 "params":[{"key":"regularization","label":"Regularization","help":"...","kind":"choice",
            "options":["truncated","tikhonov","none"],"default":"truncated"}, ...],
 "diagnostics":[{"key":"condition","label":"Condition number","unit":"","help":"...","layout":"per_line"}],
 "shapes":["square","more_controls_than_drives","fewer_controls_than_drives"],"needs":[]}
```

- Built-in laws come first, the default of each world first of all, then imported laws by name,
  version and hash. An imported law's `id` is the SHA-256 of its file.
- `params` are what a form is built from: `kind` `number`, `integer` or `per_channel` (one value
  per control channel) with `unit`, `min`, `max`, `step` and `default`; `toggle` with a boolean
  `default`; `choice` with `options`; `curve` (`[frequency, value]` points) with `unit`, `min`,
  `max`. Nothing in a screen is specific to a law.
- `diagnostics` are the numbers a law reports each loop; for a MIMO law `layout` is `scalar`,
  `per_line`, `per_control_line` or `per_drive_line` (null for a single-output law).
- `shapes` and `needs` (the optional inputs the law reads) are null for a single-output law and
  a shock law.
- A classical shock law's entry has `world: "shock_classical"`; its `diagnostics` are scalar
  values (`layout` null), at most 8, reported once per call. The built-in `fd_correct` comes
  first.
- An SRS shock law's entry has `world: "shock_srs"`, its `diagnostics` likewise. The built-in
  `srs_ratio` comes first.
- A Sine law's entry has `world: "sine"`; its `diagnostics` are scalar values (`layout` null), at
  most 8, reported once per control tick. It alone carries `live`: the keys of the number
  parameters the operator may change during a run (`sine.adjust`), each a declared number
  parameter; an empty list when there are none. The built-in `servo_blend` comes first.
- A Mixed Mode run takes a `single` law for its floor and a `sine` law for its tones, listed and checked as "Mixed Mode Laws" says; there is no `mixed` world.
- A TWR law's entry has `world: "twr"`; its `diagnostics` are scalar values (`layout` null), at
  most 8, reported once per call. The built-in `twr_correct` comes first.
- A MIMO Shock law's entry has `world: "shock_mimo"`, its `diagnostics` scalar values (`layout`
  null), at most 8, reported once per call, and `targets`: the target kinds it accepts
  (`waveform`, `srs` or both). The built-ins `inverse`, `shape` and `paired` come first, in that
  order. `default_for` lists the target kinds a law is the default for, the law a new setup of
  that kind opens on (`["waveform"]` on `inverse`, `["srs"]` on `shape`, empty on every other
  entry). `laws.import` takes a `shock-mimo` law file like any other; it runs on the simulator at
  once and drives hardware after a passing `laws.check`.
- `check.hardware` says whether the law may drive hardware: a built-in law always may; an
  imported law only with a passing check record for its exact file. Any law runs on the
  simulator. `check.message` says what stands.
- `problems` lists files in the laws folder that are not in the catalog, and why.
- **`laws.import`** reads the law file at `path` (an absolute path on the computer that runs the
  test; files reach the service by path and are never uploaded), finds which world it
  implements, checks its manifest, and copies it into the data folder under its SHA-256.
  Importing a file already in the catalog returns its entry. It needs control. Refusals:
  `bad_params` (no path, or a relative one), `not_controller`, `law_import_failed`,
  `catalog_unavailable`.
- **`laws.check`** runs a MIMO law on the simulator's check set before it may drive hardware:
  the well-conditioned 4 x 4 and 12 x 6 modal plants, the ill-conditioned 12 x 6, the rigid
  plate, the 4 x 4 with measurement noise 30 dB under the response, and the 12 x 6 on a level
  ladder from -20 dB to full level. Each case is a whole run in the controller, from
  identification to full level, with the law under the core's own guards, against a reference
  the plant can reach. The law is named by catalog `id`, or by the `path` of a law file, which
  is imported first; `params` are its parameter values by key (defaults fill the rest). Every
  case asks that every call is answered, in time, with a drive the core can use, and that no
  protection trips; then `requirement` says the rest: `converge` (every control within
  +/- 1 dB within 20 loops at full level; the square plants), `settle` (converge, or hold a
  steady drive: no control's error moving more than 0.5 dB loop to loop over the last 3 loops,
  every control within 6 dB; the plants with more controls than drives, where the documented
  methods aim at different compromises), `survive` (as `settle`, or the run ended by the core's
  own divergence judgement, reported in `trip`; the ill-conditioned plant). A law file's result
  is recorded against its SHA-256 (`recorded` true) and its entry's `check` comes back with the
  record; a passed record is what lets an imported law drive hardware. A built-in law is
  checked but not recorded: it is cleared already. While it runs, one `laws.check` event goes
  out as each case starts (`state` `running`, `result` null) and one as it ends (`state`
  `done`, `result` the case). It needs control, refuses while a run is live, and runs one at
  a time. Refusals: `bad_params` (neither `id` nor `path`, or both; `params` not an object; a
  relative path), `not_controller`, `run_active`, `law_unknown`, `law_check_unsupported` (a
  single-output law), `law_params_invalid`, `law_import_failed`, `law_check_running`,
  `law_check_not_recorded`, `catalog_unavailable`.

```
{"name":"ill-conditioned 12 x 6","plant":"modal-12x6-ill","requirement":"survive","passed":true,
 "detail":"every call answered; the core ended the run by its own judgement (...)",
 "loops_to_converge":null,"loops_at_full":2,"final_error_db":6.32,"worst_line_db":10.55,
 "settle_db":null,"trip":"The change cap held drive 4 back ...","misses":0,"rejected":0,
 "repaired_lines":0,"capped_lines":7210,"calls":5,"worst_call_ms":32.3,"budget_ms":2125,
 "gate_passed":false}
```

`misses` counts loop calls whose drive was not used, `rejected` those refused at an output
check (the core's, or the law host's at a law file's boundary), `repaired_lines` and
`capped_lines` the drive-lines the core repaired and held back, `budget_ms` a call's time
budget.

**A classical shock law.** For a law of world `shock_classical`, `laws.check` runs the classical
check set, each case a whole run under the core's guards on a simulated station: AUTO
`[-12, -9, -6, -3, 0]` dB with `[1, 1, 1, 1, 3]` pulses, advancing on a passing pulse, at most 5
pulses a level, MIL-STD-810 Method 516, a half-sine of 11 ms unless the case says otherwise. The
cases: a wire (`every_pulse`: the first pulse and every pulse pass); one mode at 400 Hz, Q 10;
the simulated station's plant (200 and 550 Hz, Q 2.5) with a haversine of 6 ms; one
mode behind a 2 ms delay (each `pass_by` 3: a pass by pulse 3 at the first level, and every
full-level pulse passes); a wire with the identified gain scaled +3 dB before inversion (`pass_by`
8, with 8 pulses allowed at the first level; a law that does not correct cannot pass it); one mode
with measurement noise 30 dB under the first level's pulse (`pass_by` 4); and a 30 dB notch at
300 Hz with its coherence under the floor (`survive`: completed, or ended by the core's own
judgement, never over the drive ceiling and never an overshoot abort). Every case first asks that
every call is answered, in time, with zero misses and zero refused outputs. A classical law is
named by `id`, or by the `path` of its file (imported first), as a MIMO law is; `params` are
checked against its manifest (`law_params_invalid`), and a case that cannot be run at all is
refused `law_check_unsupported`, naming it. A case's result:

```
{"name":"one mode","plant":"one mode, 400 Hz, Q 10","requirement":"pass_by","passed":true,
 "detail":"...","end":"completed","trip":null,"pulses":7,"first_pass_pulse":2,
 "full_level_pulses":3,"full_level_passed":3,"misses":0,"refused":0,"capped_bins":0,
 "calls":8,"worst_call_ms":1.2,"budget_ms":250,"max_drive_vpk":0.41}
```

**An SRS shock law.** For a law of world `shock_srs`, `laws.check` runs the SRS check set, each
case a whole run under the core's guards on a simulated station at 51200 Hz: a flat target on a
1/3-octave grid from 100 to 2000 Hz, Q 10, synthesized from wavelets; AUTO at -12, -6 and 0 dB,
2 and 2 pulses and then the case's own at full level, advancing on a pass, at most 12 pulses a
level, the abort band 12 dB either way, and identification in blocks of 0.2 s. The cases: one
mode at 400 Hz, Q 10; two close modes at 240 and 260 Hz, Q 30; one mode with measurement noise
30 dB under the identification burst's peak (each `within_by` 6: every band within ±3 dB of the
target by the sixth full-level pulse and on every full-level pulse after it); a wire on a ladder
of -12, -6 and 0 dB with manual moves (`each_level_within`: every band within ±3 dB after 3
pulses at each level); one mode with the identified gain scaled +3 dB before the seed
(`within_by` 8; a law that does not correct cannot pass it); and a wire with one limit channel
whose supplied gain resonates at 500 Hz under a flat bound (`limit_held`: the predicted limit SRS
at most 0.5 dB over its bound on every pulse, and the control SRS within ±3 dB outside the
notched bands). Every case first asks that every call is answered, in time, with zero misses and
zero refused outputs. An SRS law is named by `id` or by `path` as a classical law is, with the
same refusals. A case's result:

```
{"name":"one mode","plant":"one mode, 400 Hz, Q 10","requirement":"within_by","passed":true,
 "detail":"...","end":"completed","trip":null,"pulses":9,"full_level_pulses":3,
 "first_within_pulse":1,"final_worst_band_db":0.6,"worst_limit_over_db":null,"misses":0,
 "refused":0,"limited_components":0,"calls":10,"worst_call_ms":0.1,"budget_ms":250,
 "max_drive_vpk":0.41}
```

**A Sine law.** For a law of world `sine`, `laws.check` runs the Sine check set, each case a
whole run under the core's guards on a simulated station at 16384 Hz with one drive and one
control channel at 1 V per g: a flat target of 1 g peak, identification at -20 dB, no review
hold, the drive ceiling 10 V and the abort 20 V. The cases: one mode at 250 Hz, Q 50, swept
100 to 800 Hz at 1 oct/min (`through`: every judged tick at full level within 30 Hz of the
resonance within ±1.5 dB of the target; the built-in at its defaults, `blend_w` 0.5, `slow`
and a 10 ms tick, measures +1.13 dB there); the same at 6 oct/min (`within_away`: within ±5 dB through the resonance and ±2 dB more
than 30 Hz from it); two close modes at 240 and 260 Hz, Q 30, at 2 oct/min (`within` ±2 dB); a
30 dB anti-resonance at 300 Hz with its coherence under the trust floor, at 2 oct/min
(`survive`: completed, or ended by the core's own divergence trip, never over the drive
ceiling); the one mode with measurement noise 30 dB under the response, a 30 s dwell at 250 Hz
(`within` ±1.5 dB); the one mode on a manual ladder of -20, -12, -6 and 0 dB at a 250 Hz dwell,
with one live parameter changed to the middle of its range (`each_rung_within`: every judged
tick at each level within ±1 dB after the settle window); the one mode with a limit channel
whose response rises to 4 times the control's at the resonance under a flat 2 g bound
(`limit_held`: the limit channel never more than 0.5 dB over its bound, and the control point no
more than 1 dB over its target when the notch releases); and a flat wire that plays the drive
50 ms late, a hardware station's measured lag rounded up (the other stations have none), engaged
at -6 dB for a 6 s dwell at 100 Hz (`settles`: the run completes, and every tick from 2 s after
the engage on, settle window or not, is within ±0.5 dB of the target). Every case first asks
that every call is answered, in time, with zero misses and zero refused outputs; the worst call and the ticks a
cap bound are reported with every case. A Sine law is named by `id` or by `path` as a classical
law is, with the same refusals. A case's result:

```
{"name":"slow sweep","plant":"one mode, 250 Hz, Q 50","requirement":"through","passed":true,
 "detail":"...","end":"completed","trip":null,"ticks":18000,"judged_ticks":17400,
 "worst_error_db":0.42,"worst_error_hz":251.2,"worst_away_error_db":null,"rungs":null,
 "worst_limit_over_db":null,"worst_release_overshoot_db":null,"calls":18000,"misses":0,
 "refused":0,"capped_ticks":0,"worst_call_ms":0.01,"budget_ms":2.5,"max_drive_vpk":0.41}
```

**A TWR law.** For a law of world `twr`, `laws.check` runs the TWR check set (`twr-check 1`),
each case a whole run under the core's guards on a simulated station at 12800 Hz with one drive
and one control channel at 1 V per g unless the case says otherwise: a 2.0 s field-like
reference (three decaying tones over a multi-sine floor) peaking at 1 g, pads of 512 and 1024
samples (a record of 32768 samples), the drive ceiling 10 V, identification with band-limited
random at 0.125 V rms for 4 s over 20 to 2000 Hz without the review hold, AUTO
`[-12, -9, -6, -3, 0]` dB with a 10% gate and at most 8 iterations a level, a gate failure
ending the run, and playback once with adaptation off. The cases: a wire (`first_play`: every
level gates on its first play, and full level within 5%); one mode at 400 Hz, Q 10; the
simulated station's plant (200 and 550 Hz, Q 2.5); one mode behind a 2 ms delay (each
`every_rung`: every level gates within its 8, and full level within 10%); a wire behind 41
samples on a station stating a 512-frame output lookahead at 256-frame blocks, as the NI chassis
at 12800 Hz (`first_play`, and the run's lag the measured 41 frames); a wire with the identified
gain scaled +3 dB before inversion (`first_rung_by` 4: the first level gates by its fourth play,
every later level on its first; a law that does not correct cannot pass it); one mode with
measurement noise 30 dB under the response (`full_level`: within 10%); and a 30 dB notch at
300 Hz with its coherence under the floor (`survive`: completed, or ended by the core's own
judgement, `not_converged`, `divergence` or the coverage floor, never over the drive ceiling and
never an envelope abort). Every case first asks that every call is answered, in time, with zero
misses and zero refused outputs; the worst call, the clamped bins and the worst peak and bin
rise per call are reported with every case. A TWR law is named by `id` or by `path` as a
classical law is, with the same refusals; a law the check cannot build or a case it cannot run
is refused `law_check_unsupported`. A case's result:

```
{"name":"one mode","plant":"one mode, 400 Hz, Q 10","requirement":"every_rung","passed":true,
 "detail":"...","end":"completed","trip":null,"plays":11,
 "rungs":[{"level_db":-12,"plays_to_gate":3},{"level_db":-9,"plays_to_gate":1},
          {"level_db":-6,"plays_to_gate":1},{"level_db":-3,"plays_to_gate":1},
          {"level_db":0,"plays_to_gate":1}],
 "full_level_error_pct":3.1,"lag_frames":0,"lag_measured":true,"calls":3,"misses":0,
 "refused":0,"clamped_bins":0,"worst_peak_rise_db":4.2,"worst_bin_rise_db":5.8,
 "worst_call_ms":2.1,"budget_ms":1000,"max_drive_vpk":1.4}
```

`end` is `completed`, `not_converged`, `aborted`, `law_fault`, `held` (the coherence hold) or
`fault`, with `trip` the trip's kind or the law fault's reason; `rungs` lists each level and the
plays it took to gate (null when it did not gate or was not reached).
