# SRS Shock

SRS shock fires a pulse built to meet a target shock response spectrum (SRS), one pulse at a time,
and closes the loop between pulses: after each pulse is measured and its SRS judged band by band,
the control law proposes a gain for each component of the pulse, and the controller caps the gains,
builds the next drive from them and checks it before it plays. The drive is in volts throughout,
held under a drive ceiling and an abort that the operator gives.

This guide is written for the test engineer running the test. It follows the order of a run: the
station and channels, the target, the synthesis, the envelope, the schedule, identification,
control, limits and limit channels, then the run itself. SRS shock shares its run bar, station
panel, channel table, control-law panel and manual ladder with classical shock, so an engineer who
has run one will find the other familiar.

## What the Test Does

A run goes through these steps in order:

1. **Synthesis.** Before any station, the control service fits a table of components (odd
   half-cycle wavelets, or damped sines) to the target on the band grid. This is a computation,
   never a measurement: it says what pulse the test starts from.
2. **Arm.** The station opens, adopts its sample rate and passes its gate. Nothing is driven. When
   a shaker is given, the synthesized pulse is checked against its ratings first; a pulse over them
   blocks the run until the operator arms past it in their own words.
3. **Identification.** A quiet burst, its peak held under a cap in volts. The controller estimates
   how the control channel answers at each component (and every limit channel whose gain was not
   supplied). A mean coherence under the hold stops the run before any pulse.
4. **Review.** With the review hold on, the run waits with the identified gain on screen. Nothing
   fires until the operator proceeds.
5. **Seed.** The control law gives the first gains from the identified gain. With limit channels,
   the controller then plans a notch at full level where a limit channel's prediction would pass
   its bound. A full-level drive predicted over the drive abort ends the run before anything fires.
6. **Pulses.** For each pulse: build the drive from the table at the level and check it (its
   volts, its predicted peak and kinematics); bring the station up with the pulse loaded; wait out
   the time between pulses; start it; measure its SRS and judge every band against the envelope at
   the level fired; let the law propose new gains; cap them; move the ladder.
7. **End.** The schedule completes, or the run stops, aborts, fails to converge, or the law gives
   no usable gains.

The controller holds every law to the same checks, whichever law it is.

## Opening the Test

In Acquire & Control, choose **SRS** from the test types. The screen has two steps, **Setup** and
**Run**. The run bar (where the data comes from, the run's state, elapsed time, level, the pulses
fired and passed, who holds control, and Stop and Abort once a run is live) stays in view on both.
The setup's sections are tabs; the summary beside every section says what the setup holds, what
the control service worked out from it, and what is still missing, each a click from its section.

**Proceed to Run** applies the setup (with any channel table edits first) and shows the run.
**Start** arms if needed, then runs identification. The setup is checked as it is edited: every
problem shows in the section it belongs to, and a setup with errors is never accepted.

## Station and Channels

The station panel chooses the simulator or the NI hardware, as for Random. The run controls on the
channel table's first enabled **control** row and drives its first enabled **drive** row; the
section names both. Other enabled inputs can be monitored (Limits) or held under a bound (Limit
Channels).

## The Target

- **Title:** the run's name.
- **Target:** breakpoints (frequency in Hz and SRS in g, frequency rising, joined by straight lines
  on log-log axes), or one level for every band. **Read Target** reads a breakpoint table from a
  file on the computer that runs the test: two columns, frequency and SRS, separated by tabs,
  commas or spaces, with a header row and comment lines allowed. The control service reads the file
  and says what it found; a row it cannot read is named, and the setup keeps its target.
- **Band grid:** from the first band's centre to the first centre at or above the top, below half
  the sample rate, in whole, 1/3, 1/6 or 1/12 octaves, and the quality factor every SRS of the test
  is computed at (usually 10). Because the last band sits at or just above the top, the target's
  breakpoints run to that band or past it.

Beside the target, the control service draws it on its grid with the alarm and abort bands, and the
offline synthesis against it.

## The Synthesis

- **Engine:** wavelets (odd half-cycle wavelets, at rest at both ends by construction) or damped
  sines (with the terms that bring the pulse to rest).
- **Components:** one per band (the service places one at each band's centre), or a table: typed
  here, or read from a file by **Read Table** (the engine's columns named in a header row).
- **Offline fit:** at most so many passes, done when every band is within a tolerance, each pass
  moving a component at most so far. This does not set the run's control law; the law refines from
  where the fit ends. A fit that does not reach the tolerance is a warning: the test may run.
- **Recording each pulse:** the sample rate asked of the station (the run reports the rate it
  used), the ring-out measured after each pulse's drive ends, and whether each pulse's
  pseudo-velocity SRS is computed too.

Beside it, the control service draws the synthesized pulse at full level with its velocity and
displacement, and says what the fit reached: its components and passes, the worst band, the pulse's
length, peak, peak velocity, peak-to-peak displacement and what it leaves at its end.

## The Envelope

Every band of a pulse's SRS is judged against the target at the level fired. **Alarm** and
**Abort** each give how far under and how far over the target a band may be, in dB: a band outside
the alarm band alarms and the pulse does not pass; a band outside the abort band aborts the run.
The usual envelope alarms at 3 dB each side and aborts at 9 dB under or 6 dB over. **A pair for each
band** gives every band of the grid its own alarm and abort, starting from the pairs above; it is
offered once the control service has given the grid. If the grid changes, **Fit to the Grid** keeps
the bands the table has and fills the rest.

## The Schedule

- **Automatic** fires the schedule by itself, level by level. **Manual** waits for **Fire**; the
  operator steps the level 1 dB at a time or goes to a level of the table. The mode is fixed for the
  run. There are no trains or bursts in SRS shock.
- **Levels:** from the first level to full level, 0 dB, the last, each with its pulses.
- **When a level is done:** on passing pulses (the usual choice: a pulse passes when every band is
  inside the alarm band, but for a band a limit channel holds down reading low; the law keeps
  correcting until a level's pulses pass, and too many tries at one level end the run as not
  converged), or after its pulses, passed or not.
- **Time between pulses:** at least the largest of the cooling, settling and computing times,
  from the moment the station starts one pulse to the moment it starts the next. It also sets the
  control law's time budget, a share of it.

## Identification

- **Burst:** the synthesized pulse (its energy goes where the test drives) or a short broadband
  pulse.
- **Level under full:** how far under the full-level synthesis the burst is played, in dB.
- **Burst peak at most:** the cap in volts peak; a burst over it is scaled down to it. Usual: a tenth
  of the drive ceiling.
- **Block length** of the estimate, and the **gain floor**: the identified gain is floored at this
  share of its peak before the law divides by it, so a band the system barely answers is not driven
  hard.
- **Coherence it must reach:** the mean over the band. Under it the run holds before any pulse.
- **Hold for review:** the run waits after identification with the identified gain and its
  coherence on the Identification page; **Proceed** in the run bar goes on.

## Control

The **control law** panel lists the laws of the SRS world from the control service: the built-in law
first, then imported law files. It shows the chosen law's author, file hash, check record and
whether it may drive hardware, and its parameters, drawn from the law's own manifest. **Import
Law** reads a law file by its full path on the computer that runs the test. An imported law runs on
the simulator at once; it drives hardware only once its check has passed. The law and its
parameters are fixed for the run.

**Guards** the controller holds every law to: the time budget of a call and the misses in a row that
stop the run (after a miss the gains in force are kept for the next pulse), the run's pulse cap, and
the most any component's gain may rise from one pulse to the next. A law gives one gain for each
component, a number above zero; anything else is a miss.

## Limits

- **Drive ceiling** and **drive abort**, in volts peak at the amplifier's input. There is no default
  for either; give the values for your amplifier. A drive over the ceiling is scaled down to it, and
  the run says so. A drive predicted over the abort is never played: the run aborts first.
- **Peak acceleration ceiling:** a predicted response peak over it scales the pulse down before it
  fires. **Abort at, times a ceiling:** a predicted peak, or a monitored peak, at this many times its
  ceiling aborts instead of scaling.
- **Kinematic ceilings:** peak acceleration, peak velocity and peak-to-peak displacement. A pulse
  predicted past one never fires; a measured pulse past one aborts the run.
- **Monitored channels:** any enabled input with a peak ceiling, checked after each pulse or
  predicted before it.
- **Shaker and moving mass:** the shaker record, the moving masses and the residual velocity and
  displacement the synthesis may leave. Checked at arm. Without them, feasibility is not evaluated
  and the run says so.

## Limit Channels

A limit channel is an input whose response must stay under a bound whatever the control channel
asks for: a fixture's resonance, a sensitive part of the article.

- **Headroom:** how far under its bound each prediction must stay.
- For each channel, its **bound** on its SRS (one level, or one per band of the grid, a band left
  without a bound when it has none), and the **gain from the drive** to it: supplied as a table of
  frequency and g per volt, or measured by identification.

At the seed the controller plans a notch: where a limit channel's prediction at full level would
pass its bound, the drive is notched there, and the control SRS sits under the target in those
bands by what the notch takes. Before each pulse it caps every component by what the predictions
leave; a falling gain always wins. A band the notch cut, or one within half an octave of it, that
reads under the target is held there on purpose: it still alarms, but it does not stop a pulse
passing. A band over the target, or outside the abort band, still does. Once planned, the notch shows here and on the run: how many bands
it notched and the worst prediction before and after.

## The Run Record

Fields kept with the run (a run number, an operator) are listed here. Every run is recorded in a
run folder from the moment it arms, as a Random run is (`docs/run-folder.md`, "What an SRS Run
Saves"). The **Run Record** panel takes the operator, the test article, the specification document
and clause, and notes, and whether the folder keeps the time histories: every enabled input and the
drive, a shot for identification and one for each pulse. With them off, the setup, the table, the
log, the target with its envelope, the synthesis, the identification and every pulse's results are
still kept. A run whose folder cannot be written does not arm, and says which folder and why. Once
armed, the run card names the folder; when recording has a problem the card says so and the run
goes on. A finished run's end line offers **Open in Analysis**, which converts the folder and opens
its data in the Shock workspace (each pulse's time history and SRS, the target and its alarm and
abort bands, the identification), and **Copy Folder Path**. The run has no capture of its own any
more: the run folder is the one record.

## The Run

The run bar carries the verbs that apply now: **Apply Setup**, **Arm**, **Start**, **Proceed** at the
review hold, **New Run** once a run has ended, and **Stop** and **Abort**, which any connected screen
may use. **Stop** lets the pulse in flight finish and fires nothing more; **Abort** takes the drive
down at once. On the manual ladder the bar adds **Fire**, a 1 dB step down and up, and **Go to
level**. A verb that cannot run says why when it is focused or hovered: "The next pulse may fire in
0.6 s", "A pulse is playing", "At the last level of the table".

The level card beside the pages says what the run is doing in plain words ("Pulse 3 of 7 at
-6.0 dB", "Next pulse in 0.6 s", "Holding for review"), how the last pulse went ("Pulse 2 passed:
every band within the alarm band", "Pulse 1: 4 bands outside the alarm band; correcting"), its
worst band ("Worst band 630 Hz, 1.2 dB low"), the level and drive it played, the bands outside the
alarm and the abort band, its peak and displacement, and what the limit channels held back. It
also shows the identification (the burst's peak against its cap, the coherence against the hold),
the predicted full-level drive against the ceiling and the abort, and the notch. When the run ends,
the card says how, and for a protection what tripped and on which pulse; a trip found on the
prediction says that nothing was driven for it.

The pages, each drawn from the control service's views:

- **SRS:** the measured pulse's SRS against the target at the level fired, with the alarm and abort
  bands.
- **Pulse:** the measured pulse and the drive played.
- **Kinematics:** velocity and displacement of the measured pulse.
- **Identification:** the identified gain with its floor, and the coherence with the hold.
- **Limit Channels:** each limit channel's predicted SRS for the last drive against its bound and
  the bound less the headroom, and its gain, measured or supplied.
- **Components:** each component's drive amplitude in the table in force, and the gains the law
  last returned.
- **Pulses:** every pulse fired: its level, drive, worst band and its error, the bands outside the
  alarm and abort bands, the result, its peak, what the limit channels held back, and the law call
  that gave its gains. Pick pulses here (up to six) to lay them over the SRS and Pulse pages.
- **Law:** the control law's reported values on its last call, and the run in numbers.

Pages can be added, renamed and removed, and a page opens in a window of its own for another
monitor; every window keeps Stop and Abort, and Cmd or Ctrl and period aborts from any of them.

When nobody holds control, firing pauses at once. A screen that takes control within the timeout
resumes the run; otherwise it ends with a safe stop.

## How a Run Ends

| Outcome | Meaning |
|---|---|
| Completed | The schedule finished |
| Stopped by the operator | Stop: the pulse in flight finished, nothing more fired |
| Aborted by the operator | Abort |
| Stopped: nobody held control | No screen held control for the timeout, or the service shut down |
| Aborted by a protection | A band outside the abort envelope, kinematics, drive, peak acceleration, or a monitor |
| Stopped: the spectrum did not converge | Too many pulses at one level without a pass, or the run's pulse cap |
| Held at identification | The coherence was too low |
| Stopped: no usable gains from the control law | The law missed too many calls in a row, or its first gains were not usable |
