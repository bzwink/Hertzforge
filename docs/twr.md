# Time Waveform Replication

A time waveform replication (TWR) test reproduces a recorded time history at the control point. The
recording (the reference) is played as one whole record at a time: the drive for the record plays,
the response is measured, aligned and judged against the reference, and between records the control
law corrects the drive for the next play from what it measured. The level ladder, the gate each play
is judged against, the playback schedule and every abort are the controller's, never the law's. The
drive is in volts throughout, held under a drive ceiling that the operator gives.

This guide is written for the test engineer running the test. It follows the order of a run: the
station and channels, the reference, identification, control, the schedule, limits and the run's
record, then the run itself. TWR shares its run bar, station panel, channel table and control-law
panel with the other test types.

## What the Test Does

A run goes through these steps in order:

1. **Prepare.** The control service prepares the reference (it resamples it to the station's rate,
   removes a trend, high-passes away drift, scales it and tapers its ends), takes the section to
   replicate, and places it between quiet pads on a record whose length is a power of two. Every play
   plays and measures that whole record.
2. **Arm.** The station opens and adopts its rate, and the record is prepared at that rate. Nothing
   is driven. When feasibility ceilings are set, the prepared section's kinematics at full level are
   checked against them first; a section over one blocks the run until the operator arms past it in
   their own words.
3. **Identification.** Band-limited random (or a log chirp) at a low drive in volts. The controller
   measures how the control channel answers across the band, how well (the coherence), and the
   response's lag behind the drive, which aligns every play after it. A mean coherence under the one
   asked for holds the run: nothing drives until a new run.
4. **Review.** With the review hold on, the run waits with the identified system on screen. Nothing
   plays until the operator proceeds.
5. **Seed.** The control law is built and gives the first drive. On the automatic ladder, a
   full-level drive the controller predicts over the drive ceiling ends the run here, before any
   play.
6. **Each play.** The drive in force is scaled to the level, held under the clip when one is set,
   brought to rest at both ends of the record, and refused if its peak is over the drive ceiling. The
   whole record plays; each block of the response is checked against the envelope's abort band as it
   arrives. After the play the response is aligned and judged: its rms, peak and spectral error
   against the target at the level, and whether it meets the gate.
7. **Correct.** After a correction play that did not meet the gate, the law gives the next drive,
   unless the corrections are frozen. The controller checks it before it plays.
8. **Climb, then play back.** On the automatic ladder a play that meets the gate moves the run to the
   next level; once full level has gated, the converged drive plays back on its schedule. The run
   ends when playback is done; or the run is stopped, aborted, a level spends its correction plays
   without meeting the gate, or the law gives no usable drive.

The controller holds every law to the same checks, whichever law it is.

## Opening the Test

In Acquire & Control, choose **TWR** from the test types. The screen has two steps, **Setup** and
**Run**. The run bar (where the data comes from, the run's state, station time, level, the plays
judged and the playback under way, who holds control, and Stop and Abort once a run is live) stays
in view on both. The setup's sections are tabs; the summary beside every section says what the setup
holds, what the control service worked out from it, and what is still missing, each a click from its
section.

**Proceed to Run** applies the setup (with any channel table edits first) and shows the run.
**Start** arms if needed, then runs identification. The setup is checked as it is edited: every
problem shows in the section it belongs to, and a setup with errors is never accepted.

## Station and Channels

The station panel chooses the simulator or the NI hardware, as for Random. The run controls on the
channel table's first enabled **control** row and drives on its first enabled **drive** row. The
reference is in the control row's unit (g unless the row says otherwise); a reference in any other
unit is refused.

## The Reference

- **Read a reference** from a file by its full path on the computer that runs the test (CSV, TSV,
  WAV or JSON), or from pasted text. The file is read there, never uploaded. A delimited record takes
  a time column, or a value column with the rate given; **Column** picks the data column of a
  delimited file, or the channel of a WAV file. **Unit** is the record's unit, the control row's by
  default. The control service keeps the record as it was read, under its SHA-256, and the setup
  names it by that; when the setup also names the file, the file must still have that SHA-256 when
  the run is set up.
- **Preparation:** resampling to the station's rate (upward only; off, a different rate is
  refused), the detrend (none, the mean, or a straight line), a drift high-pass at or under the
  correction band's low edge, a scale for a unit correction, and a cosine taper at each end.
- **Section and record:** the whole reference, or a section of it by its first sample and length;
  the quiet lead-in and lead-out samples either side (room for the drive's pre-ring and for the
  ring-down); and the rate asked of the station, the reference's own unless given. The record is at
  most 1,048,576 samples.

Beside these the control service shows the record every play is played as (its length in samples and
seconds, the rate and the frequency step), the section's peak and rms at full level, how much of the
section's energy the correction can reach (inside the correction band and above the reference
floor) and the error floor that leaves, the section's kinematics (peak acceleration, peak velocity
and displacement, integrated after the kinematics high-pass), and four views: the section on the
record, its spectrum with the band and the floor, its velocity and its displacement. This is a
computation from the reference, never a measurement; identification can only lower the reachable
share, by its coherence.

## Identification

- **Excitation:** band-limited random, or a log chirp.
- **Drive:** volts rms; its peak is held under the drive ceiling and the clip.
- **Length**, **block length** and **band:** the identification runs for its length, is estimated
  in blocks of the length chosen (at least two must fit), and excites the band, below half the rate.
- **Mean coherence it must reach:** over the band. Under it the run holds: nothing drives, Proceed,
  Iterate and Play are refused, and only **New Run** releases the hold.
- **Hold for review:** the run waits after identification with the identified gain, phase and
  coherence and the bins the correction trusts on the Identification page; **Proceed** in the run
  bar goes on.

## Control

The **control law** panel lists the laws of the TWR world from the control service: the built-in law
first, then imported law files. It shows the chosen law's author, file hash, check record and whether
it may drive hardware, and its parameters, drawn from the law's own manifest. The law and its
parameters are fixed once the run starts. **Import Law** reads a law file by its full path on the
computer that runs the test. An imported law runs on the simulator at once; it drives hardware only
once its check has passed.

The built-in law is a frequency-domain correction: its first drive is the inverse of the identified
system applied to the reference, and after each play it adds a share of the inverse applied to the
error, on the bins the correction trusts, each bin's growth held to a limit a call. Its parameters
are the correction share, the growth limit, and a backoff for the first drive.

- **The gate:** a play at or under this rms error meets the gate at its level. **Correction plays
  at one level** is how many a level may take without meeting it before the gate-fail policy
  applies. The rms error compares the response with the target over the whole record, about their
  means: a constant offset on the control input, as a DC-coupled channel carries, is not counted.
- **What it trusts:** the correction band (inside the identification band); the coherence floor,
  under which a bin is not trusted; the inverse floor, a share of the peak gain that keeps the
  inverse from growing where the system barely answers; the reference floor, under which a bin of
  the reference is left alone; and the share of the reference's energy the trusted bins must cover,
  under which the run ends before any play.
- **Guards** the controller holds every law to: the time a call may take (a share of the longer of
  the record and 4 s; the section shows the budget in seconds), the unusable calls in a row that
  stop the run, how far the drive's peak and any of its bins may rise from one call to the next, the
  first drive's predicted response peak against the target's, and how far a play's measured lag may
  drift from the identification's before the run ends. With the built-in law, each rise cap is at
  least its growth limit plus 1 dB. A drive of the wrong length, with a value that is not a number,
  over a cap, or predicted to answer over the envelope's abort band is unusable, as is a late answer:
  the drive in force plays again.

## The Schedule

- **Automatic** corrects at each level until a play meets the gate, then moves to the next; once
  full level has gated, playback runs by itself. **Manual** waits between plays for the operator:
  **Iterate** plays one correction play, the level steps 1 dB or goes to a level of the table, and
  **Play** begins playback once the level has gated. The mode is fixed for the run.
- **Levels:** in dB, from -60 to 12, rising; the last is full level.
- **When a level does not meet its gate:** end the run (not converged), or hold at the level for a
  level move or an abort.
- **Playback:** once, a number of times, or for a time of section played (the section played that
  long over its length times, rounded up). The control service gives the number of playback plays.
- **Keep correcting during playback:** off, the converged drive replays unchanged; on, the law is
  called after each playback play too.

## Limits

- **Drive ceiling**, in volts peak at the amplifier's input, at most 10 V. There is no default; give
  the value for your amplifier. A drive whose peak is over it is refused and the run aborts; it is
  never clipped to fit. On the manual ladder a level move whose drive would pass it is refused, and
  the level and the drive stay as they were.
- **Per-sample clip:** holds each sample under it; the law sees the drive as clipped.
- **Drive rms limit** and **response rms limit:** a drive or a response over its limit aborts the
  run.
- **Envelope:** the alarm and abort bands around the target at the level, a share of the target's
  peak. Outside the alarm band raises the alarm. Outside the abort band for the samples in a row
  given, during a play or after it, the play is cancelled with the station's own stop and the run
  aborts.
- **Divergence:** correction plays in a row at one level, above the gate, each with its error up by
  more than the share given, end the run.
- **Feasibility:** ceilings on the prepared section's peak acceleration, peak velocity and
  displacement at full level, and the high-pass the kinematics are integrated after. Over any, arming
  is blocked until the operator acknowledges it in words, which the run log keeps. A ceiling left off
  is not checked.

## The Run's Record

Every run is recorded in its run folder under the program's local data folder
(`docs/run-folder.md`, "What a TWR Run Saves"); there is no separate capture to
switch on. The Run Record panel takes who ran it, the article, notes and the
document and clause it is run to, and whether the folder keeps the time
histories. Fields kept with the run (a run number, a serial number) are listed
here too, and travel in the controller's own report.

From the moment the run arms, the folder holds the setup, the channel table,
the law and the run's log. It then holds, written as the run goes:

- every enabled input and the drive as played, as one shot for the
  identification and one for every play: each play's record and the rest after
  it, the play a protection ended the run on and one a stop cut short included,
  as far as the station took them;
- the reference as imported and the target it was prepared into at full level;
- the identification's transfer function and coherence;
- each play's level, its rms, peak and spectral error, whether it met the gate
  and how it ended, written as the play is judged, the last play included;
- every call of the control law and the drive it returned. The law is called
  at the seed, after an iteration play that misses its gate, and after a
  playback play only when **Keep correcting during playback** is on, and never
  while corrections are frozen, so a play that met its gate leaves no call.

With the time histories left out, the folder keeps everything but the shots.
The **Records** on the Plays page come from the control service's memory, for
the most recent eight plays; a finished run opens in Transient Analysis from
the run card with **Open in Analysis**.

## The Run

The run bar carries the verbs that apply now: **Apply Setup**, **Arm**, **Start**, **Proceed** at the
review hold, **New Run** at the coherence hold or once a run has ended, and **Stop** and **Abort**,
which any connected screen may use. **Stop** lets the station finish what it already holds (at most
its lookahead and a block), then brings the drive down along a 250 ms taper; **Abort** does the same
with a 25 ms taper. Between plays, either ends the run at once, and nothing plays after either.
Beside them:

- On the manual ladder, between plays and until playback begins: **Iterate**, **Play** once the level
  has gated, a level step down and up of 1 dB, and **Go to level**. The level moves within the
  table. On the automatic ladder the level verbs are offered only at a level held for its gate; a
  move there releases the hold and the run goes on from the new level.
- **Freeze** holds the law's corrections off from the next play (a play in flight is corrected as
  it would have been): the drive in force plays again until **Resume Corrections**.

A verb that cannot run says why when it is focused or hovered: "A play is in flight", "Once a play
at this level meets the gate", "At the last level of the table", "Take control to iterate".

The level card beside the pages says what the run is doing in plain words ("Correcting at -6.0 dB,
iteration 2 of 8", "Playing 3 of 15 at full level", "Waiting for Iterate or Play at -12.0 dB",
"Holding: the identification did not reach its coherence floor"). It shows the play in flight with
its kind and level and a bar of how far through the record the station has played it, the level and
its place in the table, the iteration against the level's budget, the playback against its schedule,
the last play's rms error against the gate, the drive in force at the level against the ceiling, and
the identification (its coherence against the one asked for, the share of the reference it covers,
and the lag every play is aligned at). When the run ends, the card says how, and for a protection
what tripped, at which play and level.

The pages, each drawn from the control service's views:

- **Replication:** the last play's response over the target at its level, across the record.
- **Error:** the response less the target.
- **Spectra:** the target at the level, the last response and the drive it played, in the correction
  band.
- **Identification:** the identified gain and phase, the coherence against its floors, and the bins
  the correction trusts.
- **Convergence:** each play's rms error against the gate.
- **Law:** the control law's values on each call, and the run in numbers.
- **Plays:** every play: its kind, level and iteration, its rms, peak and spectral error and whether
  it met the gate, the drive and response peaks, how far its lag drifted from the identification's
  (shown as not yet measured until the alignment check has measured it), the law call that made its
  drive and the values the law reported. **Records** opens a play's target, response, error and drive
  from the control service's memory; the records of the most recent eight plays are held, and an
  older play keeps its row. A play that a stop, an abort or the in-play envelope check cut short has no row; its frames
  still count in station time, and the run's end names it.

**Target**, **Drive**, **Envelope** (the response inside the alarm and abort bands) and **Scope** (the
control channel during the play in flight) pages can be added. Pages can be added, renamed and
removed, and a page opens in a window of its own for another monitor; every window keeps Stop and
Abort, and Cmd or Ctrl and period aborts from any of them.

**Time is the station's.** The run's time, each play's start and the end's time count the frames
the station clocked over every play and pad since the run armed, so the clock stands still between
plays.

When nobody holds control, no new play starts and the play in flight finishes; a screen that takes
control within the timeout resumes the run, and otherwise a play still in flight is cancelled with
the stop's taper and the run ends. The same holds at the review hold and at the coherence hold.

## How a Run Ends

| Outcome | Meaning |
|---|---|
| Completed | The playback schedule finished |
| Stopped by the operator | Stop |
| Aborted by the operator | Abort |
| Stopped: nobody held control | No screen held control for the timeout, or the service shut down |
| Aborted by a protection | The envelope's abort band, the drive ceiling (on a play, or predicted at full level before any play), the drive or response rms limit, a play's lag drifting past its allowance, the trusted bins covering too little of the reference, or a diverging error |
| Stopped: a level did not meet its gate | A level spent its correction plays without meeting the gate, under the policy that ends the run |
| Stopped: no usable drive from the control law | The law missed too many calls in a row, or could not be built or seeded; the drive in force played through the misses, then the run stopped |
| Stopped on a fault | The run or the station failed |
