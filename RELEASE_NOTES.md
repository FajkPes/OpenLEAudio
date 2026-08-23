# OpenLEAudio 1.0

The first release considered ready for everyday use. Most of the work went into
connection faults that had been mistaken for headphone firmware problems, and
into removing assumptions taken from one particular headset so the stack has a
chance on other hardware.

Settings reset to their defaults on first run: several defaults changed.

## Connection

**Lost events made failures point at the wrong layer.** `wait_for_event`
discarded every event that did not match what it was waiting for, including
`Disconnection Complete`. During the eight-second wait for a CIS to come up -
exactly when a headset is most likely to drop the link - that notice was thrown
away, so the next attempt built on a handle the controller had already
forgotten and got `0x02 unknown connection identifier`. That reads as the
controller refusing the isochronous channels; the connection had in fact ended
seconds earlier. Non-matching events are now kept; only advertising reports and
completed-packet counts are dropped. The CIS wait also watches for the ACL
dying and stops immediately with a truthful message, and a failed `LE Create
CIS` now probes `Read RSSI` to say which handle was at fault.

**Releasing the headphones during silence killed the link.** `wait_for_sound`
read the sound card and slept, and never touched the ACL - so the connection
parameter update the peer sends unprompted went unanswered and the headset gave
up. The failure only surfaced on the next write, as "codec configuration
failed", which pointed at stream setup rather than at the connection under it.
It now services the link: answers signalling, absorbs volume and battery
notifications, and notices a disconnection at once.

**That feature is also off by default now.** Nothing in the program can tell
whether another device actually wants the headphones, so it fired on every quiet
passage whether or not anything was waiting - and taking them back costs a full
stream rebuild.

**Walking out of range ended the connection instead of interrupting it.** The
supervision timeout was hard-coded to 5 s, taken from a capture of the Windows
driver. It is now a setting, default 10 s, range 2-30. It costs nothing while
the headphones are in range.

**The 1M radio usually failed to connect.** When the controller refused the
isochronous group, the error said only "could not be established" - no status,
no numbers. At 1M every packet takes twice as long on air and the configuration
no longer fits. The group is now retried with fewer retransmissions and then
with more transport latency, and a final failure reports exactly what was asked
for.

**A refused key no longer needs manual recovery.** "Encryption restore failed;
try unpairing the device" asked the user to perform something the stack can do
itself. The stale key is discarded and pairing runs again automatically.

**Automatic reconnect never ran at all, for three separate reasons.** The
controller stayed stuck in its initiating state: the cancel was sent when an
attempt timed out, but its `Connection Complete` was not waited for, so the next
attempt read that stale event as its own answer and failed instantly - which is
why reconnecting by hand worked, the gap between two clicks being long enough
for it to be discarded. A failure during stream setup returned straight to the
app and never entered the retry loop at all. And the bond did not record the
peer address type, so a reconnect aimed at "public" for a device using a random
address and never completed - indistinguishable from headphones out of range.

**Reconnect now listens instead of sleeping.** Rather than waiting three seconds
and asking once, the radio listens for almost the whole interval, so the
connection completes the moment the headphones come back. Every retry first
performs the same cleanup as a manual Disconnect - releasing ASEs, closing the
ACL - which was the real difference that made the manual path more reliable.
The policy is re-read from disk each round, so switching reconnect off takes
effect at once.

**Six connection attempts became one.** The stack used to walk through
alternative latencies, contexts and PHYs whenever the first attempt failed. It
cost up to half a minute, left the headphones configured for something nobody
asked for, and remembered the winning shape for next time - so the settings page
and the actual stream drifted permanently apart. One attempt now, driven by what
the device published.

**Disconnect no longer leaves a queued reconnect running.** Commands are
stamped, so a `connect` that was queued before Disconnect or Unpair is dropped
rather than executed, and the retry loop checks at the top of every round.

## Working with other headphones and other adapters

**The device's own QoS is now used, not just read.** The stack asked the
headphones what they wanted, printed the answer, and then sent something else.
Retransmission count and transport latency now come from the ASE alongside the
presentation delay: the highest recommended retransmission count across ASEs,
and our latency clamped to the lowest ceiling any ASE supports. The PHY falls
back to 1M only when a device does not offer 2M at all. The custom preset is
left alone - there the user is driving.

**Two streams are the default topology again.** Carrying stereo on one
isochronous channel is tidier, but it is a path no hardware here has run,
because the reference headset cannot do it. It is now used only where a device
has a single Sink ASE and there is nowhere else to put the other channel.

**Other adapters can be added without knowing any hardware ID.**
`ADD Bluetooth adapter.bat` lists the Bluetooth controllers physically present
(by USB class E0/01/01), shows which the driver package knows, offers to add the
rest, and re-signs the package. Every ID comes from Windows' own enumeration, so
an entry can never name anything but a real Bluetooth controller - the
opt-in-per-adapter property the INF is built around is intact. Setup now also
lists controllers that are present but not in the package, so an empty menu no
longer means both "not plugged in" and "never heard of it".

## Audio

**Packet loss was always zero.** What was counted were failed USB submissions,
which only ever mean "it never left this PC" - almost never what goes wrong.
`LE Read ISO Link Quality` now provides the real figures from the radio:
unacknowledged packets, packets missed against their deadline, retransmissions,
CRC errors. It sits behind a **Link measurements** setting (off / signal only /
signal and loss).

**Two causes of dropouts, neither of them the radio.** `thread::sleep` has a
15.6 ms granularity on Windows, so a request for 7.5 ms could take twice that -
one interval skipped, the next two frames back to back. It is now a
high-resolution waitable timer, accurate to well under a millisecond, affecting
only its own thread and without raising the system tick. Separately, a buffer
underrun made the loop abandon its deadline and stay out of phase; it now sends
silence, keeps the cadence, and counts underruns apart from radio loss so a
struggling PC is not blamed on the link. Latency did not increase.

**The Robust preset could never play.** Capture required the sound card's rate
to match the codec exactly, so anything other than 48 kHz failed at the audio
device. Sample rate conversion was added, which is also what makes offering
lower rates to other devices meaningful.

## Console

- **Levels off** now hides only the bass / mid / treble breakdown. Frame count,
  channel levels, delivered counts, signal and loss stay.
- **Debug off** no longer wipes the window. It stops packet detail and trims
  history to 500 lines.
- New **playing-line rate** control (every line / 1 / 2 / 5 / 15 s). The
  measurement keeps running at full speed; only the writing slows down.
- The capability summary was printed twice per connection. PAC record hex is
  now behind Debug.
- Battery percentage goes to the indicator rather than the log. The line saying
  the headphones were asked stays.

## New

- **Headphone battery**, drawn as an indicator with connection uptime beside it.
  Every Battery Service instance is read - earbuds report left, right and case
  separately - over notifications rather than polling.
- **Environment checks**: adapter still on the Microsoft stack, VB-CABLE
  missing, VB-CABLE installed but not configured, cable not the default output,
  a playback source that no longer exists. Each with a specific remedy and a
  button to the step that fixes it. The audio core refuses to start without our
  stack and says why, instead of reporting "adapter not found".
- **Visual C++ runtime detection.** Neither .NET nor the Windows App SDK brings
  it, and without it the app dies before its first window - no window, no
  message, no log. It is now part of the dependency installer and of Setup.
- **Multipoint, without vendor protocols.** Headphones busy with a phone drop
  Media from their Available Audio Contexts; that is the only vendor-neutral way
  to detect it, and it is now read. A headset that is busy is reported as busy
  instead of failing somewhere in stream setup.
- **Codec values are coloured against what the device published** - red for what
  it will refuse, amber for what it did not list (PAC records are often
  incomplete, so it is worth trying). The rate list covers everything LC3
  defines.
- **A battery icon beside the settings that move the radio**, showing the share
  of air time each one reserves, worked out from frame duration, octets, PHY,
  retransmissions and stream count. It says plainly that this is reserved
  transmission time and not a battery measurement.
- **Left/right balance**, -50 to +50, default 0. Only ever attenuates the far
  side, so it cannot distort. Applies on the next frame.
- **The battery indicator is clickable** - it asks over the air and the answer
  arrives even while music plays. Plus an optional periodic read, default every
  15 minutes, for headsets that subscribe and then never notify.
- **Headphone power saving**: the control link wakes both radios on every
  interval while carrying almost nothing. A new setting lets the headphones
  sleep through those wake-ups. It costs nothing in audio quality or latency,
  and is off by default because it changes the timing of the link.
- **`FORGET paired devices.bat`** clears the stored pairings. The old file is
  renamed rather than deleted.

## Interface

- Device rows could render as an icon, a gap and two buttons. The list was
  rebuilt with `Clear()` + `Add()` on every status message; it is now updated in
  place, bound with `x:Bind`, and a row name can never be empty.
- "Reconnecting" is shown on the main page and in Settings, not only as a log
  line that scrolls away.
- Switching the driver binding now prompts for the restart it needs, with a
  button that does it.
- The environment check runs again when Setup is opened and when Bluetooth is
  switched on, so an adapter plugged in after startup is noticed.
- Playback settings moved to the top of the third panel, now "Playback and
  application".
- Setting names that read as jargon were rewritten, and every setting has a
  description. Descriptions moved out of the rows and behind a "?" beside each
  name - a flyout rather than a tooltip, so it can be opened deliberately and
  reached by keyboard.
- Three long-standing alignment faults: toggles sat above the centre of their
  row, dropdowns were pushed right and sliders cropped, all caused by an
  invisible "saved" marker holding space. It now lives on the name row.
- The adapter switch stayed "On" when the adapter failed to start.
- Setup marks a missing adapter in red and an adapter on the Microsoft stack in
  amber, and names the specific problem on the steps that fix it.
- Console problems are headed, sorted with blocking ones first, one per line
  with the remedy indented, and are only printed when they change.
- About gained sections on timing and headphone sharing, a "what this
  deliberately does not do" list, and honest credits.

## Fixed along the way

- **The repository could not be built from a fresh clone.** `.gitignore` used
  `dev/**/bin/` to exclude .NET build output and caught `dev/core/src/bin/` with
  it - where every Rust binary lives, including the agent that owns the radio.
  Six source files were missing from the repository and nothing said why.
- **"Preferred device" was never read by anything.** It saved, it survived
  restarts, and no code consulted it; startup reconnect took whichever paired
  headset the scan reported first, so the answer changed from one boot to the
  next. It is now a list of paired devices and startup reconnect honours it.
- The band meter in the console measured only to 16 kHz and probes above Nyquist
  returned aliases that looked like real energy. It now measures to 20 kHz and
  returns nothing above Nyquist.

## Setup scripts

Every script announces its long steps before starting them and ends with a
framed FINISHED banner saying what happened. `pnputil` operations produce no
output while they work, and the window looked finished long before it was. The
driver restore also waits for Windows to finish reinstalling before checking the
result, which used to report a failure that had not happened.

## Installation

1. Download `OpenLEAudio-1.0-win-x64.zip` from this release.
2. Extract the entire ZIP to a writable directory.
3. Run `START OpenLEAudio.bat`.
4. Complete the four steps on the Setup page.

The launcher uses already installed Microsoft runtimes when available. Missing
dependencies are identified before startup and can be downloaded from their
official sources.

## Before you rely on it

- **This was developed and tested against one headset and one adapter** - a JBL
  Tune 780NC and an ASUS USB-BT600. The work described above for other hardware
  is reasoned from the Bluetooth specification and covered by tests, but it has
  not been verified on other devices. If it works for yours, or does not, that
  is worth an issue either way.
- Binding an adapter to WinUSB takes it away from the Windows Bluetooth stack
  until you restore it. Not intended for safety-critical audio.
- Use a dedicated USB Bluetooth adapter when possible.
- Keep `RESTORE Windows Bluetooth driver.bat` available until restoration
  succeeds.
- Driver signing uses a local test certificate that must be renewed every two
  years.
- VB-CABLE installation may require a Windows restart before configuration.

---

# OpenLEAudio 0.9 Beta

OpenLEAudio 0.9 Beta is the first public experimental release of a configurable
user-mode Bluetooth LE Audio stack for Windows x64.

## Highlights

- Dedicated USB Bluetooth adapter control through Microsoft WinUSB
- Adapter detection on both the Windows Bluetooth driver and OpenLEAudio WinUSB driver
- Guided driver signing, binding, status, and restoration workflow
- Automatic dependency checks with clear recovery messages
- Optional automatic download of Microsoft runtimes and the official VB-CABLE package
- Extended LE discovery, pairing, encrypted reconnect, PACS, ASCS, and CIS support
- Configurable LC3 quality, frame duration, PHY, retransmissions, latency, and channel topology
- Stereo playback through VB-CABLE with optional headset microphone routing
- Three-second reconnect action
- Automatic startup reconnect attempts every five seconds for three minutes, enabled by default
- English interface by default with optional Czech UI translation

## Included download

The release ZIP contains only the application, driver package, setup scripts,
dependency cache placeholders, recovery-data placeholder, and user
documentation. Development source, tests, captures, build output, private
machine state, and optional installers are not included.
