# Explorer

An Explorer test drives one sine tone that the operator steers by hand: its frequency, its level, a
sweep toward either edge of a band, and a stop. Nothing closes a loop. The service measures the
response and never corrects the drive from it, so the operator is the loop. The drive is in volts
peak at the drive output, from 0 V up to a ceiling the operator gives. While the tone moves, the
service keeps the response level at each frequency it has visited, the survey line, from which the
operator reads a resonance.

This guide is written for the test engineer running the test. It follows the order of a run: the
station and channels, the tone and the drive, the records, then the run itself. Explorer shares its
run bar, station panel and channel table with the other test types.

## What the Test Does

1. **Start.** The station opens at the rate asked, adopts the rate it programs, narrows the band
   under 0.45 times that rate, and starts the tone at exactly 0 V at the start frequency. Nothing is
   driven until the operator raises the level, which is why Explorer has no review hold.
2. **Steering.** Each steering verb is taken between the service's batches of two blocks and answered
   within one batch. The answer is what the service adopted: a frequency clamped into the band, a
   level clamped to the ceiling, a sweep rate clamped to the fastest sweep. The screen shows the
   answer, never the request.
3. **Measuring.** Between batches the service measures every acquired channel: its rms, its display
   spectrum, the control channel's frequency by its zero crossings, the control channel's derived
   velocity and displacement, and the survey line.
4. **End.** The operator stops the run (the drive ramps down at the drive slew), aborts it, or a
   protection ends it.

## Opening the Test

In Acquire & Control, choose **Explorer** from the test types. The screen has two steps, **Setup**
and **Run**. The run bar (where the data comes from, the run's state, the station time, the tone's
frequency and drive, any sweep, who holds control, and Stop and Abort once a run is live) stays in
view on both. The summary beside every setup section says what the setup holds, what the control
service worked out from it, and what is still missing.

**Proceed to Run** applies the setup (with any channel table edits first) and shows the run.
**Start** opens the station and starts the tone at 0 V, in one step.

## Station and Channels

The station panel chooses the simulator or the NI hardware, as for the other test types. The run
drives the channel table's one enabled **drive** row; a table with no drive row or with more than
one is refused, since the extra outputs would stay silent. Every other enabled row is acquired, and
at least one is needed. The **control row** is the first enabled control row, else the first enabled
input: the survey line, the measured frequency and the derived motion are read from it.

Each row's **abort limits** (acceleration in g, velocity in in/s, displacement in mil, peak) are the
run's: they are checked on every input sample, and a row past one aborts the run at once.

## The Tone and the Drive

- **Band:** the frequencies the tone may move in, low to high. The top must be under 0.45 times the
  rate asked. If the station programs a lower rate, the band is narrowed under 0.45 times that rate
  at start, and the run's log says so.
- **Start at:** where the tone stands at start, inside the band.
- **Rate asked of the station:** the usual is 8192 Hz. A station may program a nearby rate; the run
  adopts it, derives every number from it and shows both.
- **Drive ceiling:** volts peak at the drive output. There is no default: give the value for your
  amplifier's input. It is at most 10 V and at most the drive row's range. Every level is clamped to
  it, and a generated sample over it aborts the run.
- **Drive slew:** dB per second, usually 6. Every level change ramps at it, and so does the stop.
- **Frequency slew:** octaves per second, usually 1. The fastest sweep is 60 times it, in octaves per
  minute.
- **FFT size:** the display spectra's size, a power of two from 1024 to 16384, usually 4096 (see
  "Reading the Survey Line").
- **Steering:** how far one press moves the tone: the fine and coarse frequency steps in steps an
  octave (usually 96 and 6, that is 1/96 and 1/6 octave), the fine and coarse level steps in dB
  (usually 0.5 and 3), and the sweep rate the steering panel starts from. These stay on this screen;
  the service is told each step as it is taken.

Beside the setup, **From the Control Service** gives what the service worked out for the selected
station: the block and the output lookahead, the pipeline (how many input frames still carry the
tone from before a change at the moment it is made), how long a rise from 0 V and a stop take, how
long the service waits for a stop, the fastest sweep, the display's line spacing, and how long after
a change of frequency the first survey point comes at each edge of the band.

## The Level: Volts at the Output

The level is the drive in volts peak at the output, and only that. Nothing measures the response back
into the drive: what the article does at a given voltage is the response you read. A level in any
other unit is refused.

- **From 0 V** the drive steps once to 1% of the level (40 dB under it), then ramps at the drive
  slew. At 6 dB/s a rise from 0 V takes 6.7 s; at 30 dB/s, 1.3 s.
- **Down to 0 V** the drive ramps to 1% of where it was, then steps to 0. The run stays running,
  quiet, until a level is set again.
- A **step in dB** moves the level from the target in force. It needs a level to step from: at 0 V it
  is refused, and the screen says to set a level in volts first.

**A limit on a rising level is passed by the slew times the queue.** When the input first shows a
limit passed, the drive already written ahead of it (the pipeline, about 83 ms at 12,800 Hz on the
NI station) is still on its way. At 6 dB/s that is about 0.5 dB past the limit; at 30 dB/s, about
2.5 dB. Raise the level slowly near a limit, or set the limit lower by that much.

## The Frequency

- A frequency set by hand lands in one frame, without a break in the tone's phase, clamped into the
  band. Setting a frequency halts a sweep.
- **A downward jump at constant volts.** The drive is volts, and on most shakers the displacement for
  a given voltage grows as the frequency falls. A jump from a high frequency to a low one at the same
  level can call for much more stroke than the high frequency did. Nothing predicts this: the row's
  velocity and displacement limits act on the measured response. Lower the level before a large
  jump down.

## Running the Test

**Start** answers once the tone runs at 0 V. The level card says "Quiet: the tone is at 0 V".

The **steering panel**, beside the run's pages, steers the tone:

- **Frequency:** steps down and up by the fine and coarse steps, and **Go to** a frequency.
- **Level:** steps down and up in dB, **Set** a level in volts peak, and **To 0 V**.
- **Sweep:** **Down** or **Up** at the rate given, from where the tone stands toward that edge of the
  band. At the edge the tone parks and holds; a sweep never turns by itself. **Hold** ends the sweep
  and the tone holds where it is.

Under the panel is the service's answer to the last verb: "Answered 2000.0 Hz, clamped to the band",
"Answered 0.500 V pk, clamped to the ceiling", "Sweeping up at 60 oct/min, clamped to the fastest
sweep".

**Keys.** While the steering panel holds the keyboard focus (click it, or move to it with Tab), Left
and Right step the frequency and Up and Down the level, by the fine step, or the coarse step with
Shift; H holds a sweep. One press is one step: a key held down does not repeat, and no key moves the
tone further than one coarse step. The keys do nothing while the focus is anywhere else.

The run bar keeps **Hold**, **Clear Survey** (empty the survey line, which fills again as the tone
moves), **Stop** and **Abort**.

The **level card** says what the tone is doing: "Quiet: the tone is at 0 V", "Rising to 0.200 V pk",
"Holding 251.3 Hz at 0.200 V pk", "Sweeping up at 4 oct/min", "Parked at the top of the band,
2000 Hz", "Stopping: the drive is ramping down". Under it are the frequency and its target, the
frequency measured on the control channel, the drive against its target and the ceiling, the control
channel's rms, its velocity and displacement (peak over the last analysis pass, marked while the
integrators settle), the station time, and the survey's highest point.

The **measured frequency** is a cross-check of the tone the service commands, read from the control
channel over half a second: the time between its first and last rising zero crossings, each placed
between its two samples, against the whole periods between them. A steady tone reads to a few parts
per million. It reads "Not yet" until a tone at a level above 0 V has held its frequency long enough:
the pipeline, then the half-second window, about 0.6 s after a change. The reading then stays until
the tone moves, and updates every half second. It counts samples at the rate the service believes, so
it cannot show a station running at another rate; a meter on the drive can.

## Stop, Abort and Protections

- **Stop** is the controlled stop: any sweep ends, the drive ramps to 0 V at the drive slew, then the
  station stops. At 6 dB/s that takes 6.7 s. Once a stop has begun nothing raises or moves the drive:
  every steering verb waits and says so. The run ends "Stopped by the Operator".
- **Abort** ends the run within one batch, without the ramp; the station takes the drive down with
  its own short stop. Any connected screen may stop or abort; everything else needs control.
- A drive slew so slow that its 40 dB stop would outlast the service's stop wait (30 s, so under
  1.333 dB/s) is refused at setup.
- **Drive over the ceiling:** a generated sample over the ceiling is clipped and the run aborts.
- **Input abort limits:** a row past its acceleration, velocity or displacement limit aborts the run.
- **Motion limit:** a slower reading of the control row's derived velocity and displacement, once
  they have settled, past the row's limit ends the run with the controlled stop.
- **Faults:** an underrun, a stalled input, a lost device, a scheduling fault or a failure of the
  service's own loop end the run at once. A stall of the service's loop shows on the run view, with
  Abort open.
- **Nobody in control:** the run goes on unchanged (tone, level, and any sweep, which parks at its
  edge) for the timeout shown in the run bar's countdown, then takes the controlled stop. A service
  shutting down does the same.

Station time, not the screen's clock, times everything: the station time on the run bar and the
level card, the stop's ramp and the end.

## Reading the Survey Line

The **Survey** page draws the control channel's response level, peak, at each frequency the tone has
visited, on a log frequency axis. The line at the tone's frequency marks where the tone is; the thin
line marks the survey's highest point, named under the plot and on the level card. That is how a
resonance is found: sweep or step across a band and look for the peak.

- A point is taken once the tone has stood at a frequency for the pipeline, a short guard and three
  cycles, and never from fewer than three cycles. Low frequencies take longest: about 0.38 s at
  20 Hz against about 0.13 s at 250 Hz and up on the NI station at 12,800 Hz.
- A visit close to an existing point replaces it. The tone passing through a span, by a sweep or a
  jump, removes every older point inside it, so a second pass over a band shows the second pass.
- Points are joined by straight lines in log frequency; a jump leaves a straight line across the span
  it skipped.
- A swept point is recorded against the frequency the tone had a pipeline earlier. At 30 oct/min that
  is about 0.04 octave, which matters only where the response changes fast with frequency.

**Why a finer FFT gives a finer line.** Two points closer together than half the display's line
spacing (the rate over the FFT size) count as one visit. At 12,800 Hz an FFT of 4096 gives 3.125 Hz
lines and keeps points at least 1.56 Hz apart; 16384 keeps them 0.39 Hz apart, for a sharp resonance.
A larger FFT also makes each display spectrum take longer to fill.

The other pages: **Spectra** (every acquired channel's latest display spectrum), **Time** (the
control channel's most recent samples, on the station's clock) and **Readings** (the run in numbers).
Pages can be added, renamed and opened in a window of their own for another monitor; that window
shows the run and keeps Stop and Abort, and the steering stays on the main screen.

## What the Run Records

Each run writes a run folder as every recorded control run does (`docs/run-folder.md`, "What an
Explorer Run Saves"): the run folder's name, `explorer` unless named, under the program's local
data folder; when that name is taken, the run gets the next free one, `explorer-2`, and so on. It
holds the configuration as accepted and what it resolved to, the channel table, the station and
the rates asked and adopted, the run's log with every steering verb the run took (its station
time, the frame it took effect at, what was asked and what was adopted), every acquired input as
read and the drive as the device accepted it, the survey line and each channel's last display
spectrum at the end, and how the run ended with what it measured. The operator, the article, notes
and the specification the run was made to go in with it. The time histories can be left out
(**Record time histories** off in the setup); the rest is always kept.

The folder is written before the station is opened: a data folder that cannot be written refuses
the start, and nothing is driven. While the run goes, the screen names the folder; a problem
writing it is a notice, and the run goes on. A finished run offers **Open in Analysis**, which
converts the folder and opens its data in the analysis side's Signals workspace, and **Copy Folder
Path**. The survey line and the spectra are magnitudes with no phase, so on the analysis side they
stay in the run's record as named arrays; the time histories become signals there.
