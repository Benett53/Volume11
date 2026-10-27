# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 0.3.0 — 2026-10-27

### Added
- Updates from the settings: current version, "Check for updates" and
  "Download and install". A small dot on the settings button shows when a new
  version is out; there are no pop-ups.
- Automatic check at most once a day, switchable in the settings.
- Releases carry `SHA256SUMS` and a minisign signature. The updater installs a
  file only if both checks pass.

## 0.2.1 — 2026-10-04

### Changed
- New icon.
- One row per program. All of a program's audio sessions share its level, and
  a second session that opens later is brought to that level.

### Fixed
- While hidden, the window kept waking itself, using about 0.6 % of a CPU core
  doing nothing. Idle use is now zero.
- A second audio session from the same process was never controlled, and with
  several processes of one program a slider could move a different session
  than the one shown.
- A failed device switch, or briefly having no playback device, replaced the
  mixer with an error until the program was restarted. It is now a notice that
  clears once audio is back.
- Starting with no playback device left the program without audio control
  until restarted. It now waits for a device to appear.

## 0.2.0 — 2026-09-24

### Added
- Output device switcher: click the device name above the mixer. Each device
  keeps its own saved levels, and switching applies that device's set.

### Changed
- Configuration format 2 stores levels per device. Levels saved by 0.1.0 move
  to the device in use at the first start.

### Fixed
- An autostart entry pointing at a deleted copy of Volume11 is repointed at the
  running one. The installer also adopts an existing entry.
- Program names read from an executable's version resource could end in
  garbage, e.g. "Spotify8?FileV".

## 0.1.0 — 2026-09-21

First release.

- Per-application volume, stored by executable name and re-applied when a
  program starts playing
- Follows the default output device, event driven rather than polled
- Tray icon with mixer and settings; closing the window keeps it running
- Autostart, switchable from Task Manager
- True-black theme with seven accents, application icons read from the
  executables
- Installer with update and uninstall paths; the plain executable is attached
  for a portable copy
