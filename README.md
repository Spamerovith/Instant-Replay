# Instant Replay

Instant Replay and screen recorder for Windows, written in Rust: a background replay buffer saved with a hotkey, plus normal recording, with GPU encoding.

**Languages:** Русский · English · Українська (chosen on first launch, can be changed in General)

## Features

- Replay buffer in memory (15 s - 20 min), save the last minutes with a hotkey without stopping
- Normal screen or window recording straight to disk
- Windows Graphics Capture (monitor or window, cursor on/off), no yellow border
- Hardware encoding through Media Foundation: NVIDIA NVENC, AMD, Intel; H.264 / HEVC / AV1; CBR / VBR / CQP; up to 8K, 30-240 fps
- Audio: system sound (WASAPI loopback) and microphone on separate tracks: 1 - system, 2 - mic, 3 - combined (default)
- Overlay over the game (Ctrl+Alt+Z) with a gallery of recent clips
- Clips open in a separate [JustView](https://github.com/Spamerovith/JustView) window (speed 0.25-3×, audio track selection); without JustView a built-in player is used
- Tray icon, global hotkeys, autostart, notifications that are not captured in recordings
- Built-in update check from GitHub Releases

## Default hotkeys

| Action | Shortcut |
| --- | --- |
| Save replay | Ctrl+Alt+F10 |
| Start / stop recording | Ctrl+Alt+F9 |
| Instant Replay on / off | Ctrl+Alt+F8 |
| Overlay | Ctrl+Alt+Z |

## Install

1. Download `Instant-Replay-<version>-windows-x64.zip` from [Releases](https://github.com/Spamerovith/Instant-Replay/releases) and unpack it anywhere
2. Run `Instant Replay.exe`
3. To play clips install [JustView](https://github.com/Spamerovith/JustView/releases) and FFmpeg: `winget install Gyan.FFmpeg`

## Build

Requires Rust 1.85+ and the Windows SDK (`rc.exe`).

```powershell
cd Исходники
.\build-release.ps1
```

The exe is copied to `Instant Replay/`. Developer notes: [Исходники/DEVELOPMENT.md](Исходники/DEVELOPMENT.md)

---

**RU** - Мгновенный повтор и запись экрана для Windows с аппаратным кодированием

**UA** - Миттєвий повтор і запис екрана для Windows з апаратним кодуванням
