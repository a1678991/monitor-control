# monitor-control

Control monitor brightness and input source over DDC/CI on Linux.

Split into four crates, so the window and the command line sit on the same foundation:

| Crate | What it is |
| --- | --- |
| [`monitor-control`](crates/monitor-control) | The library: DDC/CI protocol, EDID parsing, path discovery and failover |
| [`monitorctl`](crates/monitorctl) | The command-line front end |
| [`monitor-layout`](crates/monitor-layout) | Where the desktop puts each monitor, read from GNOME, wlroots, or X11 |
| [`monitor-control-gui`](crates/monitor-control-gui) | The window, `monitorctl-gui` |

The library talks to `/dev/i2c-*` directly and has no runtime dependency on `ddcutil`. Nothing
in the workspace links a C library: the display-server protocols are spoken by pure-Rust
clients, and the window is drawn with OpenGL through `egui`.

## The problem it solves

A monitor plugged into a machine twice — HDMI *and* DisplayPort — shows up as two DRM
connectors on two I2C buses, and DDC/CI usually works over only one of them. There is a
second, less obvious layer of the same problem: a single DisplayPort connector exposes
**two** candidate buses in sysfs, and only the AUX-channel one carries DDC/CI.

```
card1-DP-2  ├─ /dev/i2c-10  AMDGPU DM aux hw bus 1   ← DDC/CI works here
            └─ /dev/i2c-5   AMDGPU DM i2c hw bus 1   ← the `ddc` symlink points here
```

Addressing a monitor by bus number, the way `ddcutil --bus=11` does, means knowing which
of those happened to work today. This library models the monitor as the thing you address
and the buses as routes to it:

- displays are grouped by EDID identity, so one physical monitor is one `Monitor`;
- every candidate route is recorded as a `MonitorPath` and probed to see what answers;
- commands automatically fail over when the route in use stops working, as it does when a
  monitor sleeps or switches input.

Probing distinguishes three failure modes, because they call for different fixes:

| Status | Meaning |
| --- | --- |
| `working` | The display answers DDC/CI here |
| `ddc-disabled` | The display replied with a null message — it is present, but DDC/CI is switched off in its OSD menu |
| `unresponsive` | Nothing answered on this bus |
| `unavailable` | The bus could not be opened, usually a permissions problem |

Telling `ddc-disabled` apart from `unresponsive` is the difference between "this cable is
wrong" and "go change one setting in the monitor's menu".

## Install

```sh
cargo build --release
install -Dm755 target/release/monitorctl ~/.local/bin/monitorctl

# The window, and the desktop entry that gives it a name and an icon.
install -Dm755 target/release/monitorctl-gui ~/.local/bin/monitorctl-gui
install -Dm644 packaging/monitor-control-gui.desktop \
    ~/.local/share/applications/monitor-control-gui.desktop
install -Dm644 packaging/monitor-control-gui.svg \
    ~/.local/share/icons/hicolor/scalable/apps/monitor-control-gui.svg
```

The desktop entry's basename has to stay `monitor-control-gui`: Wayland compositors match a
window to its icon through the application ID the window sets, and that is the one it sets.

Talking to a display needs read/write access to its `/dev/i2c-*` node. Load `i2c-dev` and
install the udev rule, which grants an ACL to whoever is logged in at the seat:

```sh
echo i2c-dev | sudo tee /etc/modules-load.d/i2c-dev.conf
sudo modprobe i2c-dev
printf 'KERNEL=="i2c-[0-9]*", TAG+="uaccess"\n' \
  | sudo tee /etc/udev/rules.d/60-ddcci-i2c.rules
sudo udevadm control --reload && sudo udevadm trigger
```

The rule is written out rather than copied from `packaging/60-ddcci-i2c.rules` so the block
works when pasted anywhere — including into a ticket for whoever has root on a machine that
has never seen this repository. It is the same rule; the packaged file only adds a comment
header. `monitorctl-gui` shows this exact block when it finds it cannot open a device node.

Adding yourself to the `i2c` group works too, but grants access to every I2C bus on the
machine rather than just the ones behind your session.

## Usage

With no selector, a command applies to **every** detected monitor.

```sh
monitorctl detect                     # monitors and the path each is controlled through
monitorctl paths                      # every route, including the dead ones and why
monitorctl brightness                 # read brightness from all monitors
monitorctl brightness set 50          # absolute
monitorctl brightness set +10         # relative step, clamped at the display's range
monitorctl brightness set 75%         # percentage of the display's advertised maximum
monitorctl contrast set 70 -m BenQ
monitorctl input                      # which input each monitor is showing
monitorctl input set hdmi2 -m DELL
monitorctl input list                 # the input names this tool understands
monitorctl capabilities -m DELL       # what the display says it supports
monitorctl vcp get 0xDF               # raw VCP read
monitorctl vcp set 0x60 0x11          # raw VCP write
```

### Selectors

Selectors may appear anywhere on the command line, before or after the subcommand.

| Flag | Matches |
| --- | --- |
| `-m, --model TEXT` | Model name contains `TEXT` |
| `-s, --serial TEXT` | Serial number contains `TEXT` |
| `-b, --bus N` | Monitor reachable on I2C bus `N` |
| `-c, --connector NAME` | DRM connector name contains `NAME`, e.g. `DP-1` |
| `-k, --key KEY` | Exact stable key, as printed by `detect` |

Prefer `--key` in scripts and saved GUI profiles. It is derived from the EDID, so it
survives replugging, reboots, and the monitor moving to a different port — unlike a bus
number, which does not.

### Global options

| Flag | Effect |
| --- | --- |
| `--json` | Machine-readable output, for scripts and GUIs |
| `--dry-run` | Show what would be sent without touching a monitor |
| `--retries N` | Retries per exchange before a path is judged dead (default 3) |
| `--sleep-multiplier F` | Scale every DDC/CI delay; raise it for flaky displays |
| `--no-group` | Treat each connector as its own monitor instead of grouping by EDID |
| `--strict-checksum` | Reject replies whose checksum does not use the spec's seed |

`--no-group` matters when two identical monitors report the same EDID. Displays that
report no serial number cannot be told apart, and `detect` says so rather than silently
merging them.

### Exit codes

`0` when every selected monitor succeeded, `1` when any failed. A partial failure still
applies the change to the monitors that worked — one sleeping display does not stop the
others.

## The window

```sh
monitorctl-gui
```

A console: one row per monitor, in the order they sit on the desk, with every brightness fader
in the same vertical column and a master at the top of that column. After a week you open it,
drag at a fixed screen position, and close it without reading anything.

```
+---------------------------------------------------------------------------+
| DESK                                                     plan from mutter  |
|        +--------------+   +------------------+   +-----+                   |
|        |(1) Dell 27"  |   |(2) DELL G3223Q   |   |(3)  |                   |
|        |#####  48% ###|   |####### 40% ######|   | 52% |                   |
|        +--------------+   +------------------+   +-----+                   |
+---------------------------------------------------------------------------+
| 0 |=| All monitors      ############|..............   47%  [=]  - + Copy   |
|      % of each display's own top          1    3  2        spread 12       |
+---------------------------------------------------------------------------+
| 1 |#| Dell U2723QE      #############|.............   48%  [DP1] - + ~ v    |
| 2 |#| DELL G3223Q       ##########:..|.............   40%  [DP1] - + ~ v    |
| 3 |#| BenQ PD2700U      ###############|...........   52%  [HD2] - + ~ v    |
+---------------------------------------------------------------------------+
| 4 screens . 3 monitors . 3 answering - one panel on two cables             |
+---------------------------------------------------------------------------+
```

Three things it does that the command line cannot:

- **A closed loop.** `brightness set 50` is guess, look up, guess again. Nobody wants 50; they
  want *as bright as the one next to it, in this light*. Writes are collapsed to the last value
  per feature, so the panel follows the pointer rather than trailing it by seconds.
- **Pointing instead of naming.** The plan at the top is the desktop's real arrangement, read
  from GNOME, a wlroots compositor, or X11. Two identical displays are told apart by where they
  are, which is the only attribute you can actually see from your chair.
- **A diagnosis instead of an error.** A monitor that will not answer says which of four things
  is true, in one line, with the fix attached.

### The master moves the desk by an offset

Dragging it from 65% to 40% moves every monitor down twenty-five points and keeps the trim you
put on one of them. A master that wrote the same percentage everywhere would undo that work
every evening. Flattening is still one click — the `=` button — and it says so.

Percentages are of each display's own maximum, which is not always 100. Equal percentages are
not equal brightness, and the window does not claim otherwise: the readout says how far apart
the monitors are, never that they match.

### When a monitor does not answer

The fader rail is what changes, because it is the widest thing in the row:

| What is wrong | The rail | What to do |
| --- | --- | --- |
| DDC/CI is off in the monitor's own menu | filled with a 45-degree hatch | change that setting |
| Nothing answers on any cable | a dashed, empty outline | wake it, or bring its input back |
| The device node cannot be opened | flat, with a padlock | install the udev rule above |
| It is showing another computer | flat, with one line through it | nothing — this is success |

Switching a monitor to another machine's input takes the control channel with it. That is the
command working, and the window says so rather than flashing an error.

### Keys

| Key | Does | Key | Does |
| --- | --- | --- | --- |
| `1`-`9` | that channel's fader | `←` `→` | one step |
| `0` | the master | `PgUp` `PgDn` | ten steps |
| `↑` `↓` | previous / next channel | `Home` `End` | minimum / maximum |
| `E` | this channel's details | `M` | hide the desk plan |
| `R` | re-read everything | `Shift+R` | re-check this monitor |
| `L` | level every monitor | `Esc` | close the window |
| `C` | copy this channel's command | `Shift+C` | copy the master's |
| `?` | the keys card | | |

`Copy as command` produces a line such as `monitorctl brightness set 44% --key DEL-428C-F1W36H3`
for a compositor keybinding — the stable key, never a bus number, because the bus number is the
thing this project exists to stop people hard-coding. Once a setting is repeatable, it belongs
in a keybinding rather than in this window.

### Environment

| Variable | Effect |
| --- | --- |
| `MONITORCTL_GUI_THEME` | `light` or `dark` to pin the theme; otherwise it follows the desktop |
| `MONITOR_LAYOUT_SOURCE` | `mutter`, `wlr-output-management`, `randr` or `sysfs`, when auto-detection picks the wrong one |
| `MONITORCTL_GUI_TRACE` | Print every value written to a display, with `RUST_BACKTRACE=1` for the call site |

The window remembers its size, the grouping switch, and whether the desk plan is showing. It
does not remember brightness levels: those live in the displays, and a window that wrote
remembered values over them at startup would be changing hardware nobody asked it to touch.

Nothing runs when the window is closed: no daemon, no tray icon, no schedule, and no traffic on
any bus. An open window is idle too — the only background work is the arrangement poll, which
touches no display.

### Known issue on X11

Closing the window under X11 prints a panic from `winit` on the way out
(`TranslateCoordinates` on a window that has already been destroyed, winit 0.30.13). Everything
the window did has already happened by then, but the window's size is not saved on that path.
Wayland does not go through the code that panics.

## Replacing the shell scripts

```sh
# bridghtness.sh 50  →
monitorctl brightness set 50

# sw-y   (DELL to HDMI-1, 0x11)  →
monitorctl input set hdmi1 -m "DELL G3223Q"

# sw-hdmi2  (DELL to HDMI-2, 0x12)  →
monitorctl input set hdmi2 -m "DELL G3223Q"

# sw-w   (ddcutil --bus=11 setvcp 0x60 0x0f)  →
monitorctl input set dp1 -m "DELL G3223Q"
```

The bus number in `sw-w` is what this tool removes the need for: `--bus=11` was correct
only until something renumbered the buses.

## Library

```rust
use monitor_control::{DdcConfig, MonitorManager, ValueChange};

// Enumerates from sysfs; generates no I2C traffic, so it is cheap enough for startup.
let mut manager = MonitorManager::discover(DdcConfig::default())?;

for monitor in manager.monitors_mut() {
    // Finds a working path on demand, probing only as far as it needs to.
    match monitor.set_brightness(ValueChange::Relative(-10)) {
        Ok(value) => println!("{}: brightness {value}", monitor.name()),
        Err(e) => eprintln!("{}: {e}", monitor.name()),
    }
}
```

`cargo run -p monitor-control --example gui_backend` is a fuller sketch of the shape a GUI
wants; [`monitor-control-gui`](crates/monitor-control-gui) is the real one.

### Notes for a GUI

- **Discovery is free, probing is not.** `MonitorManager::discover` only reads sysfs, so it
  is safe on the UI thread. `probe_all` and every get/set blocks for tens of milliseconds
  per exchange — put them on a worker.
- **`ensure_path` beats `probe_all`** when you just want to set brightness: it stops at the
  first route that answers instead of testing them all.
- **Read the maximum from the display.** `VcpValue::maximum` is not always 100; use it for
  slider bounds rather than assuming a range.
- **Build input menus from `parsed_capabilities`.** It reports the exact inputs a display
  accepts, so the menu offers only values that will work.
- **Switching input cuts the wire you sent the command over.** Moving a monitor to another
  machine's input takes the DDC/CI path with it, so the follow-up read fails. That is the
  command succeeding, not failing.
- **Disable the `serde` feature** if you do not want it: `default-features = false`.

## Where the monitors are

[`monitor-layout`](crates/monitor-layout) answers the question this library deliberately does
not: where the desktop puts each screen. A DRM connector knows nothing about the arrangement,
because the arrangement belongs to the display server.

| Source | Where it works |
| --- | --- |
| `zwlr_output_management_v1` | sway, Hyprland, river, Wayfire, labwc |
| `org.gnome.Mutter.DisplayConfig` | GNOME, on Wayland and on X11 |
| X11 RandR | any X11 session, including XWayland |
| `/sys/class/drm` | anywhere, but the arrangement is then a guess and says so |

```sh
cargo run -p monitor-layout --example arrangement
```

Each is asked in turn and the first that answers wins. Wayland before X11: under a Wayland
session the X11 answer comes from XWayland, which mirrors the compositor's layout rather than
being the source of it — and drops the EDID, invents the physical sizes, and reports every
rotated display as upright.

Matching a screen to a monitor survives the fact that DRM says `card1-HDMI-A-1` where a display
server says `HDMI-1`, and that one monitor on two cables is two screens but one `Monitor`.

## Testing

```sh
cargo test          # 79 tests, no hardware required
cargo clippy --all-targets
```

The protocol tests assert against frames captured from real monitors and cross-checked
against `ddcutil`, including the null-message and trailing-noise cases that a synthetic
fixture would miss.
