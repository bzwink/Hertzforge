# MIMO Random

MIMO Random drives several shakers at once so that several responses on the test article match a
reference together: each control's power spectral density (PSD) and, for every pair of controls,
the coherence and phase between them. One run controls up to 24 drives and 72 control channels,
in any shape: as many controls as drives, more controls than drives, or fewer.

This guide is written for the test engineer running the test. It follows the order of a run:
channels, reference, control settings, schedule, backend, the prediction, the run views, and what
is kept afterwards. The single-output Random test and MIMO Random share their run bar, schedule
editor, station panel and level ladder, so an engineer who has run one will find the other
familiar.

## What the Test Does

The reference is a spectral density matrix (SDM): on every frequency line, a square matrix whose
diagonal holds each control's PSD and whose off-diagonal terms hold the cross spectrum of each
pair. Coherence and phase are read from those cross terms. The controller's job is to find, line
by line, the drive SDM (the PSD of every drive and the cross spectra between drives) that makes
the measured response SDM equal the reference.

A run goes through these steps in order:

1. **Noise floor.** The drives are off. The controller measures the control channels so it knows
   what is noise.
2. **Identification.** Uncorrelated random on every drive at once, at a level below the
   reference. The controller estimates the frequency response function (FRF) matrix from every
   drive to every control, each control's multiple coherence with the drives, and the condition
   number of the FRF on every line.
3. **Gates.** The multiple coherence and the condition number are checked against their limits.
   A failed gate holds the run.
4. **Prediction.** The control law is run once against the identification, and the page shows
   what it would ask of each drive and leave on each control. Nothing drives at test level until
   the operator engages.
5. **Control.** The level schedule runs: the ladder of levels, each held for its dwell, then full
   level for its time. Every loop the controller estimates the response, hands the law what it
   needs, checks the drive the law returns, applies its own caps and limits, and plays the next
   drive.
6. **Stop.** A ramped stop, an abort, a protection trip, a law fault, or a safe stop when the
   controlling screen is lost.

The law works entirely at full level. The controller refers each measurement to its full-level
equivalent before the law sees it and scales the law's drive to the scheduled level afterwards, so
every law behaves the same way on the ladder and no law can raise the level.

## Opening the Test

In Acquire & Control, choose **MIMO Random** from the test types. The screen has three steps,
**Setup**, **Predict** and **Run**, shown in the breadcrumb at the top. The run bar (state, elapsed
time, level, time at level, and Stop and Abort once a run is live) stays in view on every step.
The setup has five sections as tabs: **Channels**, **Reference**, **Control**, **Schedule** and
**Backend**. The summary beside every section lists what each section holds and what is still
missing before a run, and each item opens its section.

**Apply Setup** sends the setup to the control service, which checks it and holds it for the run.
**Proceed to Run** applies the setup (with any channel table edits first) and moves on. The setup
is checked as it is edited, so every problem shows at once in the section it belongs to; a setup
with errors is never accepted, and the one before it stays in place.

## Channels and Roles

The Channels section edits the same channel table rows the rest of Acquire & Control uses. Each
row gets a role:

| Role | What it is | What it needs |
|---|---|---|
| Drive | An analog output to one shaker amplifier | A ceiling in V rms, the peak clip level in V, an optional abort level, and optionally the shaker record and that shaker's own moving mass |
| Control | A response the reference is written on | Unit, sensitivity, and its alarm and abort bands in dB below and above the reference (by default 3 dB and 6 dB each way) |
| Limit | A monitored response the controller holds under its own ceiling | The PSD ceiling at full level, and the margin above it that aborts (by default 3 dB) |
| Monitor | A response that is recorded and shown only | Unit and sensitivity |

A limit channel is a monitor row with a limit entered. Control names must be distinct, since the
reference and its tables are kept by name. The test article mass, entered at the foot of the
table, is split equally across the shakers for the feasibility check.

The wiring of each channel (the device, the physical channel, IEPE) lives under Backend with the
station, as in single-output Random.

### Physical and Virtual Control

The **Control Space** panel chooses what the reference is written on:

- **Physical.** The reference has one row and column per control channel. This is the usual
  choice.
- **Virtual.** The reference is written on named degrees of freedom (DOFs) derived from the
  controls, for example the six rigid-body motions of a table. The law then sees only the virtual
  space.

Two transformations connect the spaces:

- **The response transformation** maps the measured controls to the virtual DOFs. It has one row
  per DOF and one column per control channel. It must have full row rank, so there can be no more
  DOFs than controls.
- **The drive transformation** is optional. It maps virtual drives to the physical outputs, for
  shakers that are ganged or driven as a differential pair. It has one row per output and one
  column per virtual drive, and must have full column rank. The controller expands the law's drive
  through it before applying the caps, the ceilings and synthesis.

Both are entered as a labeled grid. A block copied from a spreadsheet pastes straight in, a CSV
file with labels can be imported, and **Start From Identity** fills the grid with ones on the
diagonal to edit from. The control service reports each transformation's rank and condition
number (largest singular value over smallest) beside the grid. A rank too low is an error; a
condition number above 100 is a warning, because a badly conditioned transformation magnifies
measurement noise into the controlled DOFs.

The rigid-body builder under Reference fills the response transformation from geometry, and
switches the test to virtual control on the six rigid-body DOFs.

## The Reference

The Reference section has four tabs, one per way of entering the reference. Whichever way it is
made, the reference is one object: the SDM on every line of the test's frequency grid, each
control's alarm and abort lines, the band, and a record of how it was made. Every route ends in
the same validation:

- every value finite and every dimension consistent,
- labels present and distinct,
- an imported full matrix Hermitian (each term below the diagonal the conjugate of its mirror),
- every line positive semi-definite (explained below),
- a positive PSD on every control inside the band and zero outside it,
- tolerance lines in order (each lower line under its upper one, abort lines outside alarm lines,
  and the reference inside its own alarm lines),
- transformations of full rank.

Each refusal names what failed and what to do about it.

The **Reference Preview** beside every tab shows what the controller computed: every PSD on one
plot, the smallest eigenvalue on each line against the refusal threshold, and the coherence and
phase of the pair chosen under **Pair shown**. Its provenance panel says how the reference was
made, from what file, and with what assumptions.

### Positive Semi-Definite: What It Means and the Repair

A set of PSDs, coherences and phases can be entered that no set of real signals could ever have.
With two controls any coherence from 0 to 1 is possible. With three or more, the pairs must also
agree with one another: if control 1 is fully coherent with control 2 and control 2 with control
3, then control 1 is fully coherent with control 3, and the phases must add up. A matrix that
breaks this has a negative eigenvalue on that line. It is called not positive semi-definite, and
no drive can produce it.

The controller checks every line. When some line fails, the **Lines No Signal Can Have** panel
lists how many lines fail, the worst frequency, and the smallest eigenvalue as a fraction of the
line's largest. It also describes the repair it offers: every negative eigenvalue is set to zero
and the line is scaled back so each PSD is exactly what was entered. The PSDs therefore stay as
given and the coherences give way. The panel lists, for each pair, the coherence entered and the
coherence after the repair, at the frequency where the change is largest, so it is plain what the
reference becomes.

**Accept the Repair** takes the repaired reference, and the acceptance is recorded in its
provenance. The other choice is to change the coherence or phase of the pairs listed. Very small
negative eigenvalues from rounding (smaller than a millionth of the line's largest) are left
alone; a reference with an eigenvalue more negative than a thousandth of its line's largest is
refused at setup unless the repair has been accepted.

### Import

**Import a Reference** reads a file by its full path on the computer that runs the test (the
control service reads it there, which may not be the computer showing the screen). It reads:

| File | Layout |
|---|---|
| A reference folder | The folder this program writes: `reference.bin` and `reference.json`, holding the matrix, its tolerances, any transformation and its provenance, checked against its content hash. Laid out in `docs/reference-file.md` |
| A specification `.npz` file | `cpsd` (lines by channels by channels, complex), `f` (the frequency of each line in hertz), and optionally `warning_upper`, `warning_lower`, `abort_upper` and `abort_lower` (lines by channels; `NaN` where there is no line) |
| A CSV table | A header of `frequency_hz` and one column per control, one row per frequency, PSDs in the control unit squared per hertz. An optional second CSV, given under **Pair table**, holds `frequency_hz, row, column, coherence, phase_deg` |

Rules every import follows:

- **No interpolation.** Each frequency in the file must fall on a line of the test's grid; a file
  with a frequency off the grid is refused, naming the first such frequency. Grid lines the file
  does not hold are zero, so a file may cover only part of the band.
- **A coarser file is refused.** A file at twice the test's line spacing would leave every other
  line in the band at zero. The message says to set the test's frame to the file's spacing.
- **Convention.** This program writes a cross spectrum as `G_ij = E[X_i X_j*]`, so a positive phase
  in `G_ij` means channel `j` lags channel `i`. Rattlesnake and SDynPy write the same form, and so
  does the analysis package's writer of this layout, so the `.npz` import reads every cross term as
  it is. Much analysis software writes the complex conjugate; a file made that way is conjugated on
  request, and the provenance records which was done. The CSV layout is this program's own and is
  read as written.

Other formats go through the analysis package's converter, which writes a reference folder to
import:

- A specification `.mat` file (MATLAB v5 or v7.3, the same variables with the axes transposed:
  `cpsd` channels by channels by lines, each limit channels by lines):
  `hertzforge.io.reference_convert.from_mat(path, folder, df_hz=..., lines=...)`.
- A `.hforge` file holding a random specification:
  `hertzforge.io.reference_convert.from_hforge(path, folder, df_hz=..., lines=...)`.

`df_hz` and `lines` are the test's line spacing and number of grid lines. The converter applies the
same matching rules, conjugates every cross term from the form the analysis package stores (the
complex conjugate of this program's) to this program's, records that in the provenance, and refuses
early what the controller would refuse.
Its `repair=True` applies the same repair described above and records it.

### Breakpoints

The breakpoint editor is the single-output profile editor, one table per control (or per DOF under
virtual control). Pick the control with the arrows or the drop-down above the table; **Copy This
Table to All** copies the table shown to every control. Breakpoints are entered at full level.
Under physical control the bands shown are that control's own, the same as under Channels.

Below the tables, the **Coherence and Phase** table sets each pair: squared coherence from 0 to 1,
and the phase of the first control against the second in degrees. Each is a constant or a set of
points joined by straight lines in frequency. Pairs not listed take the default chosen for the
reference: independent (coherence 0) or fully coherent (coherence 1, phase 0). The cross term on
each line is the square root of the two PSDs times the coherence's square root, at the phase given.

Entering autospectra, coherence and phase, rather than a raw matrix, is what lets the controller
judge realizability and offer the repair instead of asking the engineer to judge it by hand.

### Rigid Body

For a six-DOF test on a rigid table or fixture. The **Sensor Geometry** table takes each control
sensor's position (X, Y, Z) and sensing direction, the reference point the six DOFs are written
at, and the length unit. The controller forms the kinematic matrix that maps the rigid-body motion
at the reference point to each sensor's reading, and its pseudo-inverse becomes the response
transformation onto X, Y, Z, RX, RY and RZ. At least six sensors are needed, and the geometry must
resolve all six DOFs (an all-Z layout resolves only three); otherwise the tab says which rank the
geometry reached.

With the fit made, the tab shows its condition number and the units: translations in the sensors'
unit, rotations in that unit per length unit. The **Checked by Others** column shows how much of
each sensor's reading the other sensors can check, from 0 to 1. A sensor near 0 is the only one
in its direction: if it sits on a part that does not move rigidly, the fit cannot show it, and the
column marks it **Unchecked**. Adding a sensor in that direction is the remedy.

The reference is then entered in DOF space with the breakpoint tables (six PSDs, and the pairs'
coherence and phase), and the test controls the six DOFs.

### From Field Data

A reference can come from measured records, from the field or from flight. The route runs through
the Signal Processing module: open the records there, compute the full cross-spectral density matrix of the
control sensors (in the same order as the controls here, on the test's line spacing), export it as a
random specification `.npz`, and import that file under Import. **Open the Signal Processing Module** and **Go
to Import** on the tab lead to the two ends of the route. The import's conjugation matches what the
Signal Processing module writes, and the validation and repair apply as for any import, so a matrix estimated
from too few averages, or combined from several records in a way that broke its definiteness, is
caught before the test.

## Control Settings

### The Control Law

The **Control Law** panel lists the laws that can run this test, each with its name, version, and
whether it is built in. Six are built in, the first five written from the published literature:

| Law | What it does | When to choose it |
|---|---|---|
| Pseudo-inverse, closed loop (the default) | Starts from the regularized pseudo-inverse drive, then every loop corrects a lower-triangular factor of the reference from the measured error, autospectra first. Every drive it asks for is realizable by construction. Each drive's PSD moves at most a set amount per loop | Most tests |
| Pseudo-inverse, open loop | The regularized pseudo-inverse drive, computed again every loop from the plant estimate alone: least squares with more controls than drives, minimum norm with fewer | To see what the plant does with the drive the FRF implies; with the FRF updated, it follows the plant |
| Shape-constrained (SVD) | Holds the drive on each line to the directions the plant responds to most (its leading right singular vectors), and runs the default law's correction inside that constraint | When the unconstrained drive is too high: much lower drive for some loss of fidelity in the cross terms between modes |
| Buzz-test matched | Keeps the reference's autospectra and takes each pair's coherence and phase from the identification response, the structure's own response to uncorrelated drive; pairs below a coherence floor are left independent. Then runs the default law on that reference | When the reference's cross terms are unknown or should be what the structure does naturally |
| Minimum drive | Keeps the reference's autospectra as far as a trade-off against drive power allows, and takes its cross terms from the drive that needs the least power to produce them (the Pareto-optimal target of Coletti, Davis and Schultz; Smallwood's minimum input trace at the exact-control end). The trade-off is picked at the knee of drive against residual, or set. Then runs the default law on that target | When the cross terms are free to choose and drive is the cost: the target that needs the least shaker power for the autospectra, at a trade-off you can see |
| Band average | Solves for one flat, correlated drive per fractional-octave band so that each control's band-averaged response matches the reference's band average, then corrects each band's gain from the measured response. Averaging the FRF over a band before inverting it keeps identification noise out of the drive | When a per-line inverse asks for too much drive or follows the noise in the FRF: lower drive and a smoother drive spectrum, for a line-by-line error that stays a decibel or two inside a band with resonances in it. It can also follow a plant that changes with level, with its FRF update on and the test's FRF mode at update |

The details card under the list describes the chosen law, its author, its file hash and whether
it is cleared for hardware. Its parameters appear below as a form built from the law's own
declaration, each with its range: for the default law, the regularization (truncated, Tikhonov or
none) and its amount, the convergence factor (0.1 to 1; lower is slower and steadier), the largest
rise and fall per loop, the rank tolerance, and the error weighting.

**Import Law** adds a law file by its path. An imported law runs on the simulator at once; on
hardware it needs a passed law check (see "The Law Check Before Hardware" below). Single-output and
MIMO laws are listed apart, and a single-output law is refused here.

### FRF During the Run

- **Hold** (the default): the law uses the FRF from identification for the whole run.
- **Update**: the FRF is estimated again every loop from the drive played and the response
  measured, and blended with identification's (the blend sets the weight of each loop's estimate,
  0.5 by default). An updated FRF is sound only in the directions the drive actually excites, so
  it follows a plant that changes with level but can wander where the drive is weak. The Drive
  and Plant page shows the live estimate. A law that reads the multiple coherence is given the
  loop's own alongside, in place of identification's.

### Estimation

The frame: the sample rate (the station may run at a nearby rate; the run reports the rate it
uses) and the frame length, which together set the line spacing. Then the window (Hann or
rectangular), the overlap (half a frame is usual with Hann), the frames averaged in one control
loop, and the averaging memory, the number of loops the estimate remembers (1 uses only the latest
loop). More frames per loop make each loop's estimate steadier and each loop longer.

### Identification

The identification level (of the control response, relative to the reference; uncorrelated random
on every drive steps up until the response is there, never above the drives' ceilings), the
identification frames (at least twice the number of drives, ten times is better), and the
noise-floor frames, measured with the drives off before identification.

**Hold for the prediction after identification** is on by default: nothing drives at test level
until the prediction has been read and the run engaged. With it off, the run engages by itself
when the gates pass.

### Gates

Checked once identification is done:

- **Coherence floor.** Each control's multiple coherence over the band must reach it (default 0.5).
  A control the drives cannot explain, such as a broken sensor or a response dominated by noise,
  fails here.
- **Condition limit.** The FRF's condition number on every line must stay under it (default
  1,000). Above it, the inverse the laws rely on is not trusted: two drives look alike to the
  controls on that line, and a small error asks for a large drive.

A failed gate holds the run until it is overridden in the operator's own words or the setup is
changed.

### Caps and Budgets

The controller holds every law to these, whichever law it is:

| Setting | Default | Meaning |
|---|---|---|
| Rise cap, fall cap | 3.5 and 6 dB per loop | The most any drive's PSD may rise or fall on a line from one loop to the next |
| Law time budget | 0.25 of a loop | How long the law may take on one call |
| Misses before a stop | 3 | Unusable law calls in a row (late, failed, or with a drive the controller refuses) that end the run in a controlled stop |
| Diverging loops to abort | 5 | Consecutive loops the controller judges the error to be growing before it aborts |
| Level ramp, shutdown ramp | 3 s each | How fast the level moves between rungs, and down at a stop |

A cap on one drive is applied so that the drive matrix stays realizable and every pair's coherence
is unchanged. Limit channels are protected the same way: where a limit channel's predicted PSD would
pass its ceiling on a line, the whole drive on that line is scaled down.

## Schedule

The single-output schedule editor, unchanged. Each level is in dB relative to the reference, in
rising order, and the last is full level. With **Automatic**, the run steps up through the levels,
holds each for its dwell, holds full level for its time, and ends complete. **Add Level** adds a
rung; the level ramp under Control sets how fast the level moves between rungs.

## Backend

The station panel and the channel wiring, as in single-output Random, and **Feasibility per
Drive**. Before identification the drive the test needs is not known, so each drive reads **After
identification**; the Predict page gives the numbers. A drive with no shaker record is not checked
for feasibility.

The simulator and the NI station are the two backends. A run on the simulator is marked SIM on
every screen and in every record.

## Running the Test

Start the run from the run bar. The status beside it reads the phase in words: the noise floor,
identification, holding for review, engaged, full level.

### Noise Floor and Identification

Both run without the operator. The noise floor is measured with the drives off; identification
steps the uncorrelated drive up to its level and measures for the frames set. Stop and Abort work
throughout.

### The Gates and the Override

When identification is done, the Predict step comes forward. **Identification Gates** shows the
smallest multiple coherence against its floor and the largest condition number against its limit,
and whether they passed.

If a gate failed, the run holds and the Override button is offered. An override asks for the
operator's own words, which are kept with the run and printed in its report. The other choice is
to stop, change the setup (a sensor, a drive, the band, the limits) and start again.

### The Prediction Page

Before anything drives at test level, the law is run once against the identification and the page
shows:

- **Drive per Output.** Each drive's predicted rms against its ceiling, as a bar with the ceiling
  as a tick. A drive over its ceiling would be capped during the run, leaving its controls short of
  the reference.
- **Error per Control.** Each control's predicted rms error over the band against its bands, with a
  verdict in words. The darker band is the alarm band, the lighter the abort band.
- **The Law.** The law and its parameters, which can be changed here. **Predict Again** reruns the
  prediction with the law and parameters shown, feeding the predicted response back as if measured,
  so a closed-loop law shows where it would settle. The law shown here is the one that engages.

With more controls than drives, the drives cannot reach every reference exactly; the page says so,
and the predicted error per control shows where the reference will be met only in the
least-squares sense.

**Engage** starts the schedule. While the prediction asks a drive for more than its ceiling,
Engage is blocked: lower the reference, raise the ceiling if the shaker allows it, or change the
law (the shape-constrained law usually needs much less drive).

### During the Run

The Run step shows the run view: the level card, the drives table, and the run pages. The **level
card** reads the level the controller is at, the loop count, how many controls have an rms error
beyond their alarm and abort bands, the largest rms error and where it is, the time at this level
and the time left, and the gates measured at identification. The **drives table** lists each drive's
rms against its ceiling, and its cap factor this loop (1 when the controller did not hold it down;
a warning mark when it did).

The run pages are chosen from the **Page** drop-down. **Add Page**, **Rename** and **Remove Page**
build a set of pages, and **Updates** sets how often the pages refresh (up to 10 a second). A page
asks the control service only for what it draws, so a 72-control test costs no more to watch than
a small one.

### The Error Heatmap

The overview, and the page the run view opens on. Controls (or DOFs) run down the side, frequency
across; each cell is the measured PSD's error against the reference in dB on that line.

How to read it:

- **Lightness carries the size.** A cell near the plot's background is on the reference. The
  further the error, the further the cell's lightness moves from the background: blue below the
  reference, orange above.
- **The bands are breaks, not shades.** Each row's alarm and abort bands are steps in the scale, so
  a cell beyond alarm is visibly a different step, and a cell beyond abort is also **hatched**.
  Meaning never rests on hue alone: the sign of an error is in the number under the cursor as well
  as the color.
- **The right-hand column** is each control's rms error over the band. Beside it, a filled square
  and a count mark lines beyond abort, and an open triangle and a count mark lines beyond alarm.
- **The cursor** reads the control, the frequency, the error in dB and the band it falls in, in
  words. The legend shows the bands of the row under the cursor.
- **Order.** **Setup Order** or **Largest Error First**. The toolbar counts the controls with any
  line beyond alarm or abort.
- **Click a row** to open that control in the matrix grid.

A second mode shows the coherence error: for each control and line, the rms over its partners of
the measured squared coherence minus the reference's (0 to 1). It shows whether the cross terms are
converging as well as the autospectra.

A converging test reads as rows fading toward the background, loop by loop.

### The Matrix Grid

A grid of small plots for a window of up to eight controls:

- **The diagonal** is each control's PSD, measured (solid) against the reference (dashed) and its
  alarm (dotted) and abort (dash-dot) lines, drawn as steps over each line's width.
- **Above the diagonal**, coherence from 0 to 1 against the reference's. **Below**, phase from -180
  to 180 degrees against the reference's. **Cross Spectrum** switches the off-diagonal cells to
  cross-spectrum magnitude.
- **One cursor frequency** runs across every cell, with the readout above the grid (the pair, the
  frequency, the measured value and the reference). The `[` and `]` keys step it.
- **The window.** The arrows beside **Controls 1 to 8 of 72** scroll through the controls (Page Up
  and Page Down from the keyboard). **Pin** keeps a chosen control first in the window while the
  rest scroll (P from the keyboard).
- **Click a cell** to open it full size with the single-output plot's tools.

### Drive and Plant

What the controller is doing and what it is working with:

- **Drive.** The PSD of the chosen drive, and every drive's rms against its ceiling.
- **Frequency Response.** The FRF from a chosen drive to a chosen control, magnitude and phase.
- **Singular Values.** The FRF's singular values per line. A singular value far below the others
  marks a direction the shakers can barely push.
- **Condition Number.** The FRF's condition number per line with the gate's limit.
- **Drive Coherence.** The coherence between two chosen drives.
- **Drive Caps This Loop.** Each drive's cap factor.
- **Law Diagnostic.** Any diagnostic the law declares, chosen by name.

### Geometry

The control sensors and the drives in three dimensions, from the sensor positions and directions
entered for the test. Each sensor is colored by its control's current error on the heatmap's scale,
and the table beside it lists every control's error and band in words. Drives are arrows at their
positions and directions, longer for more drive rms. Under virtual control on the six rigid-body
DOFs, the sensors move with the rigid-body motion the DOF rms values describe. **Amplitude**,
**Surface** or **Wireframe**, and **Ghost** (the undeformed shape) set how it is drawn.

### Stopping and Aborting

**Stop** (ramped) brings the level down over the shutdown ramp and ends the run. **Abort**
(immediate) takes the drive to zero at once; its keyboard shortcut is shown on the button. Both are
on the run bar on every step of the screen, and any screen connected to the control service can
stop or abort a run, including one that is only watching. If the screen in control is lost during
a run, the controller makes the same ramped stop by itself, and the run ends as a safe stop.

The controller also ends a run on its own:

| Protection | What trips it |
|---|---|
| Envelope | A control's PSD beyond its abort band; beyond its alarm band, the run alarms |
| Limit | A limit channel's measured PSD beyond its ceiling by the abort margin |
| Divergence | The controller's own judgement that the error is growing, over the set number of loops |
| Drive voltage | A drive's rms beyond its abort level; peaks are clipped at the peak level |
| Kinematic and input overload | As in single-output Random |
| Law fault | Too many unusable law calls in a row: the run ends in a controlled stop |

Coherence and phase errors are shown and recorded; they do not trip the run. The report says how
the run ended and why.

## Records

Every run, on the simulator or on hardware, writes its run folder under the local data root
(`Hertzforge_Data` in the home folder unless the data root is set elsewhere) from arm, as every
recorded module does (`docs/run-folder.md`). The folder is named for the time the run armed and its
run id (`20261113-140000_mimo_random_m-0003`); a folder is never written over. The arm is refused
(`record_unwritable`) when the record cannot be written. It holds:

- the manifest, the channel table as armed and the setup as accepted and resolved: the reference
  and where it came from (inline, or a reference file with its checksum), the control and drive
  channels, the band, the loop and averaging settings, the limits and guards, the level schedule,
  the setup hash and the seed,
- the law: its identity, every parameter value, its guards, and for an imported law the law file
  itself,
- the log, each entry written as it is made, so the events survive a run that does not end
  cleanly,
- every enabled input as read and every drive as the device accepted it, the noise floor and the
  identification included,
- the reference as the run used it, in the control space, with each control's alarm and abort
  lines,
- the identification: the transfer function matrix, the multiple coherences, the condition
  numbers, the noise, response and drive spectral matrices and the response-to-drive cross
  spectrum, with the setup hash they were measured on,
- each loop's drive spectra and summary (errors, drive rms, what the guards did, timing), and each
  judged loop's control spectra and error against the reference,
- the full control and drive spectral matrices at the first and last loop at full level and every
  30 seconds,
- every call made to the law, with the inputs that change from call to call and the drive it
  returned,
- how the run ended, with its report: the gates and any override, each control's final error, each
  drive's rms and smallest cap.

Nothing is written on the thread that makes the drive: each loop's numbers are copied and handed to
the record's own writer, so a slow disk never delays a loop. The screen shows the folder and any
problem writing it, and opens the run in Analysis, where the folder converts into an ordinary
project with its channel table, its specification and its results (`docs/run-folder.md`, "What a
MIMO Random Run Saves"). A run that did not finish converts what it wrote, and says it is
incomplete.

### Identification Reuse

A re-run on an unchanged setup can skip the noise floor and identification. The control service
saves a run's identification set to a folder (`mimo_random.identification.save`) and loads a saved
set before the next start (`mimo_random.identification.load`); the start then goes straight to the
gates and the prediction. A set is loaded only onto the setup it was measured on: the channels,
their sensitivities, the transformations and the frequency grid are compared by hash, and a set
from a different setup is refused with a message to identify again or restore the setup. These are
messages of the control service's protocol (`schema/PROTOCOL.md`).

### Replay

The run service's own records (`hertzforge_mimo_random::records`, kept for a run made with the
service directly, as its own tests make them, and for saving and loading an identification set) can
be replayed through their law: every recorded law call is fed again, in order, to the law as
recorded (a built-in by its name and parameter values, or the law file kept with it), and each
drive it returns is compared with the recorded one. A replay that reproduces the
run shows the law did what the record says; a replay with changed parameters shows what the change
would have done to the same inputs. Replay is a function of the run service
(`hertzforge_mimo_random::replay`).

## The Law Check Before Hardware

The built-in laws are cleared for hardware. A law imported from a file runs on the simulator at
once, but arms on hardware only when a law check has passed for that exact file. The check runs the
law against a fixed set of simulated test articles and keeps a record beside the law file, named
by the file's hash, so changing one byte of the file sends the law back to the simulator. The law
details card shows each law's check state. Hertzforge checks that a law's output is a well-formed
drive and keeps the drive inside the controller's limits; it does not check that an imported law
controls well, which is what the check and the simulator are for. `docs/control-laws.md` describes
the law files and the check.

## What the Simulator Is For

The simulator plays a cross-coupled modal plant: real resonances, with every drive reaching every
control, sized to the channel table (4 controls by 4 drives, 12 by 6, 24 by 24, or 72 by 24; a
table of another size is refused, naming the sizes it has). Use it to:

- rehearse a setup, its reference and its schedule before hardware time,
- learn how a law and its parameters behave, with no risk to an article,
- run an imported law before its law check, and see its faults handled,
- check that a large reference, its transformations and the run views behave at full scale.

A simulated run is marked SIM everywhere it appears and is never a hardware result.
