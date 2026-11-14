# Mixed Mode

A Mixed Mode test plays several parts together on one drive and controls them together: a floor of
broadband random, sine tones, and narrow bands of random added over the floor. A tone or a
narrowband stands still at a frequency, sweeps between two, or follows another part at a ratio (a
rotor's harmonics, a gear's mesh frequency). The parts list decides the test: a floor and tones is
sine-on-random, a floor and narrowbands is random-on-random, and all three is both. Every part is
controlled to its own target and judged against its own alarm and abort bands; one level schedule
moves every part by the same dB.

This guide is written for the test engineer running the test. It follows the order of a run: the
station and channels, the floor, the tones and narrowbands, the schedule and identification,
control, limits and the run record, then the run itself. Mixed Mode shares its run bar, station panel,
channel table and control-law panel with the other test types, its floor, ladder and identification
with Random, and its tone tracking and tone law with Sine.

## What the Test Does

A run goes through these steps in order:

1. **Arm.** The station opens, adopts its sample rate, builds the analysis lines and the control
   tick at that rate, checks every part's motion again, and passes its gate. Nothing is driven. When
   a shaker is given, the predicted peak of the sum is checked against its ratings first; a test over
   them blocks the run until the operator arms past it in their own words.
2. **Identification.** Random's broadband identification: a noise burst under full level that
   measures how the control channels answer across the band, how well (the coherence), and the
   delay between the drive and the response. It gives the floor law its plant and each tone its
   feed-forward at the tone's frequency. A band coherence under the one asked for holds the run
   before anything plays at test level.
3. **Review.** With the review hold on, the run waits with the identified gain and its coherence on
   screen. Nothing drives until the operator proceeds.
4. **The floor alone.** Both laws are built, and the floor engages alone at the schedule's first
   level, with Random's soft start. It plays alone until it has equalized (its estimate holds the
   control degrees of freedom and its rms is within 1 dB of its reference), or until the most time
   given for it, after which the parts come in anyway and the log says so.
5. **The parts come in.** Parts time starts. Each tone and narrowband whose window is open and that
   is switched on ramps in over the part ramp; a tone comes in from its feed-forward amplitude.
6. **Each tick.** Every part's clock moves on; each tone's oscillator steps without a break in its
   phase; each tone is read on every control channel through its own tracking filter, the control
   channels are combined, and the tone law is called once for each tone that is on and settled. The
   core caps how far each tone's drive may move.
7. **Each loop.** Each tracked tone is taken out of the control signal, and the floor is estimated
   on what is left (the residual). The lines a standing tone or a narrowband covers are left out of
   the floor's average. The floor law is called on the floor alone, each narrowband's level is
   servoed in the band's own moving frame, and the random part (floor and narrowbands) is made for
   the next segment. Every part is judged against its bands.
8. **End.** The ladder finishes and every part ramps down together; or the run is stopped, aborted,
   held at identification, or a law gives no usable drive.

## Opening the Test

Mixed Mode is in the Single Output group of the test types. Its screen has two steps, Setup and Run,
under one header line that carries the run's state, the level, parts time and how many parts are on,
where the data comes from (SIM or the hardware) and who holds control. Setup is in sections, with a
summary beside every one; a section with something left to fix says how many. **Proceed to Run**
applies the setup and shows the run.

## Station and Channels

The station and the channel table are shared with every test type. The run controls on the table's
enabled control rows and drives on its first enabled drive row. Rows are picked by their number, and
an edit to one picked row is made to every picked row.

## The Floor

The floor is Random's breakpoint profile, in g²/Hz at full level: each row a PSD or a slope from the
row before, with the alarm and abort bands of the whole profile and, on any row, bands of its own. Its
band, from the first row to the last, is the test's band: every tone and every narrowband must stay
inside it through the whole of its motion. The rows are saved to and opened from a breakpoint table
file. The reference the service computes (the floor and each narrowband where it starts, at full
level, with its alarm and abort lines) is drawn under the table.

## Tones and Narrowbands

Each part is a row of its table: up to 12 tones and up to 12 narrowbands. A row holds:

- **Id and name.** The id is the part's name on the wire and in every plot and log line (letters,
  digits, `_` and `-`); the name is for people, and a run-end names the part by it ("Tone 3 abort").
- **On.** Whether the part plays from its window's start. A part switched off stays off until it is
  switched on during the run.
- **Moves.** **Stands still** at a frequency, for its whole window or for a number of seconds or
  cycles. **Sweeps** between two frequencies at a rate, in octaves a minute or hertz a second,
  bouncing at the ends, from a start and in a direction, until its window closes, for a count of
  one-way sweeps, for a time, or whichever comes first. **Follows** another tone or narrowband at a
  ratio: it reads its leader's frequency at the same moment, through the leader's turns and holds.
- **A tone's amplitude:** a constant in g peak, or a Sine profile against frequency (acceleration,
  velocity or displacement rows) read at the tone's frequency. A profile is saved to and opened from
  a breakpoint table file, and must cover the tone's whole motion.
- **A narrowband's level and width:** the density added over the floor between its edges, in g²/Hz,
  and its full width, in hertz or as a share of its center (a band given by share widens as it
  climbs). The band's reference is the floor's reference plus its level, line by line.
- **Alarm and abort,** in dB either side. A tone's are its own (usual 1.5 and 3 dB). A narrowband
  takes the floor's profile bands unless it is given its own; either way they are widened to the
  band's own scatter at the degrees of freedom it holds. A tone's alarm and abort judge its reading
  averaged long enough that the reading's own scatter is a third of the alarm band: a tone well above
  the floor is judged at once, a small one over up to 8 seconds, and the setup's prediction and the
  tone's row state how long. A tone too close to the floor for even 8 seconds to settle its reading is
  judged against tolerances widened to that scatter, and its row says so.
- **On at and off at,** in parts time: when the part ramps in and out. Parts time is 0 when the floor
  has equalized, counts on the station's clock, and stops on a hold.

The service checks every part's motion against the floor's band and keeps two tones at least two
tracking widths apart through the whole of both motions (two tones that follow one leader are
compared at the same moment). A refusal is said in one line under the row it concerns. The service's
derived values are in the summary: each part's mean square over the motion it makes, the total and
the random part's rms, the predicted peak acceleration, velocity and displacement of the sum, the
lines left out of the floor's average for the whole run, the closest pair of tones and how close
they may come, and each tone's tracking verdict. The tones' amplitudes across their motion are drawn
under the tables.

## The Schedule and Identification

The schedule is Random's ladder: levels in dB under full level, each held for its dwell, then full
level for its time, the dwells and the full-level time counted in parts time. In the manual ladder
the run moves only when the level is stepped. Every part moves by the same dB, together, over the
level ramp.

Identification steps up to its level under the floor's reference over a number of averages, from a
starting drive and at most a capped drive, and must reach the coherence floor; with the review hold
on, nothing plays until the operator proceeds.

## Control

- **The floor's law** is chosen from the single-output Random laws and is called each loop on the
  floor alone, on the lines no tone or narrowband covers; lines left out are given the error of the
  clean lines either side, so the law neither chases a part nor loses the floor's mean.
- **The tones' law** is chosen from the Sine laws: one choice, one instance for each tone, called
  each tick. A parameter the law marks live can be changed during the run, for every tone at once.
- Each law panel says whether the law may drive hardware in its role. A built-in law may; a law file
  needs a passed check in its own world and a passed Mixed Mode check in its role.
- **Control point:** how the control channels combine (weighted average, the lowest, the highest),
  and their weights.
- **Tones and their lines:** the tracking width every tone is read through (usual 0.5 Hz), the
  width either side of a standing tone left out of the floor's average (usual 2 Hz), and the control
  tick (usual 10 ms).
- **The floor's loop:** the rate asked of the station, the FFT length (the lines are the rate over
  it apart, and no wider than the tracking width), the frames a loop, the control degrees of freedom,
  the correction damping, and the most time the floor is given alone.
- **Ramps:** the part ramp (one part in or out), the soft start, the level ramp and the shutdown ramp.
- **Where the identification is trusted** for a tone's feed-forward and its law.
- **Guards:** the core holds each law to these whatever law it is. The floor law: the most a drive
  line may move in a loop, the share of a loop a call may take, the unusable calls in a row that stop
  the run. Each tone's law: Sine's guards.

## Limits

- **The summed drive's ceiling and abort,** in volts peak, with no default: the random part and
  every tone together are held at or under the ceiling, judged on every frame, and a sample at or over
  the abort aborts the run.
- **The random part's own** drive ceiling, abort and clip, acting on the floor and narrowbands before
  the tones are added.
- **The floor's aborts:** clean lines outside its abort band in one loop, and diverging loops in a
  row.
- **The kinematic monitor,** with a shaker record: the response's velocity and displacement, read
  on the raw samples with the tones in, against the shaker's ratings.
- **Limit channels:** a limit lowers the random part only, never a tone. A tone that drives a limited
  channel too hard is caught by that channel's abort, the kinematic monitor and the drive ceiling.
- **Shaker ratings:** the predicted peak of the sum, checked at configure and at arm.

## The Run Record

Every run is recorded, from the moment it arms, in a run folder of its own under the local data
folder, named for the time it armed and the run (`<YYYYMMDD-HHMMSS>_mixed_<run id>`); no run writes
over another. The Run Record panel on the setup takes the words kept with the run (the operator,
the article, notes, and the document and clause the test is run to) and whether the record keeps
the time histories. The run's screen shows the folder and its recording line while the run goes,
and once it has ended opens the run in Analysis or copies the folder's path.

The folder holds the channel table the run armed with, the setup as applied and what it resolved
to (the floor's breakpoints, every tone and narrowband with its sweep, dwell or follow and its
level, the schedule, the limits and the control settings), both control laws with their guards, the
run's log as it was made, every enabled input and the drive as the device accepted it from the
identification on, the identification's transfer function and coherence, the floor's spectra at each
level change and at the end, each tone's tracked level, phase and frequency, each narrowband's and
the floor's level as the run went, every call made to the floor's law and each tone's law, and how
the run ended. `docs/run-folder.md` ("What a Mixed Mode Run Saves") lists it file by file.

The record is written on its own thread, as the run goes: a slow or full disk never slows or stops
the run. When the data folder cannot be written the run does not arm, and says why. When the disk
holds less than the space kept free, the run goes on without its time histories and says so.

## The Run

The Run card carries the verbs that apply now: **Arm**, **Start**, **Proceed** at the review hold,
**New Run** once a run has ended, and **Stop** and **Abort**, which any connected screen may use.
**Stop** ramps every part down together, each linearly in amplitude from the level in force to zero
over the shutdown ramp (about 6 dB down halfway through); **Abort** takes the drive down at once.
Beside them:

- **Hold** freezes the test's progress: parts time, every part's schedule and window, and the level
  clock stop, and every part stays at its frequency and level, still under control. **Resume**
  starts them again.
- The **Ladder** switches between automatic and manual; in manual the level steps by the step
  chosen or goes to a level of the schedule.
- The **parts table** switches a tone or a narrowband on or off during the run, once the parts have
  come in, as a ramp over the part ramp. A part switched off holds its own clock. The floor cannot
  be switched off, and a part whose window has closed or whose schedule has ended cannot be switched
  back on (its switch says so); a refusal is said in one line under the part's row.

The level card says what the run is doing in plain words ("Floor alone at -12.0 dB, equalizing",
"Every part at -6.0 dB", "Held: every part at its frequency and level"), the level the drive is at
against the level commanded, parts time and the time at this level, the composite rms against its
reference (shown, not judged), the summed drive, each law's unusable calls and capped loops or ticks,
and the identification. The parts table shows, for every part, what it is doing ("Sweeping up at
1 oct/min, sweep 2 of 4", "Follows Rotor at 2×, sweeping up at 1 oct/min", "Waiting for its window,
from 40 s", "Its schedule ended"), its frequency or center, its target against its measured value
with the error in dB (for a tone, the judged error its alarm and abort use, with the single reading beside it,
smaller), its drive, and its state: in band, settling, limiting, alarm, off, waiting or
ended, each with its own mark and word. A sweep's rate is in the unit its schedule states: oct/min
for a log sweep, Hz/s for a linear one. Under the floor's row are its lines (clean, in alarm, over
abort) and the fewest degrees of freedom any clean line holds; under a narrowband's, its width (and
its share of its center when it is sized that way), the degrees of freedom of its level and its
servo gain.

The pages, each drawn from the control service's views:

- **Control:** the control spectrum with the tones in and the **Residual** with each tracked tone
  taken out, each against the reference (the floor and each narrowband where it is now) with its
  alarm and abort lines, drawn as computed, flat across each line's bin. The lines left out of the
  floor's average are shaded and named for their part.
- **Parts:** the parts table, and each part's frequency over the last minute.
- **Tones:** each tone's tracked amplitude against its target and the alarm and abort lines it is
  judged on (widened, for a tone judged against widened tolerances).
- **Narrowbands:** each narrowband's level in its own moving frame against its reference and its
  widened alarm and abort lines.
- **Drive:** the random part of the drive as made for the next segment.
- **Identification:** the identified gain with where the tones may trust it, and the coherence.
- **Law:** the floor law's and each tone law's values on their last calls, and the run in numbers.

**Response** and **Time History** plots can be added from **+ Plot**. A per-part plot shows every
part of its kind, or the parts chosen in its options. Pages can be added, renamed and removed, and a
page opens in a window of its own for another monitor; every window keeps Stop and Abort.

When nobody holds control, the run goes on; if no screen takes control within the timeout, it ends
with the controlled stop.

## How a Run Ends

| Outcome | Meaning |
|---|---|
| Completed | The ladder finished; every part ramped down |
| Stopped by the operator | Stop: every part ramped down |
| Aborted by the operator | Abort |
| Stopped: nobody held control | No screen held control for the timeout, or the service shut down; every part ramped down |
| Aborted by a protection | A part outside its abort band (the run-end names it: "Tone 3 abort"), the floor's envelope, divergence, a lost control point, a frequency the station cannot play, the shaker's kinematics, the summed drive's abort, the random part's drive abort, or an input's watchdog |
| Held at the coherence check | The identification's coherence was too low |
| Identification could not reach its level | The identification needed more drive than it may use |
| Stopped: no usable drive from a control law | The floor law or a tone's law missed too many calls in a row, or could not be built; that role's drive in force was held through the misses, then every part ramped down |
| Stopped on a fault | The loop or the station failed |
