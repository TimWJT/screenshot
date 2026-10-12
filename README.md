# Screenshot

A tiny screenshot tool for Windows. Press a key, drag a box, draw on it, paste it anywhere.

One 500 KB exe with no runtime and no installer required. It sits in the tray using about 4 MB of memory and does nothing until you press the shortcut.

## Download

### [→ Download the latest version](https://github.com/TimWJT/screenshot/releases/latest)

On that page, open **Assets** and pick **one** file:

| You want | Download the file ending in |
|---|---|
| **Install it** (Start menu entry, starts with Windows) | **`_x64-setup.exe`** |
| **No install**, just run it | **`_x64-portable.exe`** |

Needs Windows 10 or 11, 64-bit.

### "Windows protected your PC"?

That's expected. The app is free and not code-signed, so Windows doesn't recognise the publisher. To run it:

1. Click **More info** (small text under the message).
2. Click **Run anyway**.

You only see this once.

## How to use it

Press **Alt+F1** or **Print Screen**, or left-click the tray icon. The screen freezes and dims.

- **Drag** to select an area, or **click** to grab the window under the mouse.
- Draw on it with the toolbar that appears under the selection.
- Press **Enter** to copy it and close. Paste it into anything.

Running `screenshot.exe` also starts a capture, and puts the app in the tray if it wasn't there already.

## What it does

| | |
|---|---|
| **Region, window or full screen** | Drag a box, click a window (it highlights as you hover), or press `F` for the whole monitor and `Ctrl`+`A` for every monitor. |
| **Magnifier** | A zoomed view of the pixels under the cursor while you select, with coordinates and the colour's hex code. Arrow keys move the cursor one pixel at a time. |
| **Adjust** | Drag the edges or handles to resize the selection, drag inside it to move it, or nudge it with the arrow keys. |
| **Draw** | Rectangle, ellipse, arrow, line, pen, highlighter, text, numbered steps and pixelate. Eight colours plus a colour picker, three sizes (the mouse wheel fine-tunes), filled or outlined shapes. |
| **Edit** | Move, recolour, resize or delete anything you drew with the select tool. Undo and redo everything. |
| **Copy** | `Enter`, `Ctrl`+`C` or double-click. |
| **Save** | `Ctrl`+`S` saves a PNG straight to `Pictures\Screenshots` and closes; click the notification to see it. `Ctrl`+`Shift`+`S` picks a name, folder, or JPEG. Optionally every copied screenshot is also saved there automatically. |
| **Pin** | `Ctrl`+`P` leaves the screenshot floating on top of everything. Drag it around, scroll to zoom, `Ctrl`+scroll for transparency, double-click or `Esc` to close. |
| **Delay** | Tray menu → *Capture after a delay* gives a 3, 5 or 10 second countdown, for menus and tooltips that close when you press a key. |
| **Pick a colour** | Before selecting, press `C` to copy the hex code of the pixel under the cursor. |
| **Multi-monitor and high DPI** | Captures every monitor at full resolution. Toolbar and drawing sizes follow each monitor's scale. |

## Keyboard

While selecting:

| Key | Does |
|---|---|
| `Enter` / `Space` | Select the highlighted window |
| `F` | Select the whole monitor |
| `Ctrl`+`A` | Select every monitor |
| `C` | Copy the colour under the cursor |
| Arrow keys | Move the cursor 1 px (`Shift`: 10 px) |
| `Esc` / right-click | Cancel |

Once something is selected:

| Key | Does |
|---|---|
| `V` | Select and move |
| `R` `E` `A` `L` | Rectangle, ellipse, arrow, line |
| `P` `H` | Pen, highlighter |
| `T` `N` `B` | Text, numbered step, pixelate |
| `1` `2` `3` / mouse wheel | Size |
| `F` | Filled shapes on/off |
| `Shift` while drawing | Straight lines at 45° steps, squares and circles |
| `Ctrl`+`Z` / `Ctrl`+`Y` | Undo / redo |
| `Delete` | Delete the selected drawing |
| Arrow keys | Move the selection or the selected drawing (`Shift`: 10 px) |
| `Enter` / `Ctrl`+`C` | Copy and close |
| `Ctrl`+`S` | Save to the screenshots folder and close |
| `Ctrl`+`Shift`+`S` | Save as… |
| `Ctrl`+`P` | Pin to screen |
| right-click | Start over (if nothing is drawn yet) |
| `Esc` | Close without copying |

While typing text, `Enter` makes a new line, `Ctrl`+`V` pastes, and `Esc` or clicking elsewhere finishes it.

## Settings

Right-click the tray icon → **Settings…**

- **Capture shortcut**: any key combination with `Ctrl` or `Alt`, or a function key on its own. Defaults to `Alt`+`F1`.
- **Print Screen key also captures**: on by default. This takes the key over from Windows' Snipping Tool while the app is running.
- **Start with Windows**
- **Save folder**: where `Ctrl`+`S` saves. Defaults to `Pictures\Screenshots` (created if missing; if OneDrive backs up your Pictures, that's the OneDrive one).
- **Also save every copied screenshot to this folder**

Settings are stored in `%APPDATA%\Screenshot\settings.ini`.

If another program already owns the shortcut (`Alt`+`F1` is also the NVIDIA overlay's screenshot key), Screenshot catches it with a keyboard hook instead, so it still works.

## Command line

```
screenshot.exe              capture now (and start the tray app if it isn't running)
screenshot.exe --full       capture with the whole monitor already selected
screenshot.exe --delay 5    capture after a 5 second countdown
screenshot.exe --tray       just start in the tray
screenshot.exe --settings   open Settings
screenshot.exe --quit       close the tray app, pinned screenshots and any open capture
```

## Why it's light

- Pure Rust against the Win32 API. Drawing uses Direct2D and DirectWrite, images go through WIC, so everything it needs ships with Windows.
- The tray process only handles the shortcut, the tray icon and settings. Each capture runs in a short-lived second process that exits when you're done. Direct2D and the graphics driver hold on to over 100 MB after drawing a full-screen image and only give it back when the process exits, so this keeps the always-running part at about 2 MB. The next capture process is started ahead of time and waits, already loaded (about 2 MB more), so pressing the shortcut only has to wake it. Its first frame is drawn with plain GDI while the graphics driver starts up on another thread, then Direct2D takes over.

## Build from source

Needs [Rust](https://rustup.rs) with the MSVC toolchain (Visual Studio Build Tools).

```bash
cargo build --release
```

The exe lands in `target/release/screenshot.exe`, with the C runtime linked in (see `.cargo/config.toml`), so it needs nothing else installed. `cargo test` renders the editor off-screen to `target/scene-*.png`, so the UI can be checked without opening a window.

To build the installer, install [Inno Setup 6](https://jrsoftware.org/isinfo.php) and run:

```bash
iscc /DAppVersion=0.1.0 installer\screenshot.iss
```

The icon is drawn by code: `node tools/make-icon.mjs` regenerates `assets/icon.ico`.

## Project layout

| | |
|---|---|
| `src/main.rs` | Tray icon, shortcuts, keyboard hook, single instance, starting captures |
| `src/overlay.rs` | The capture editor: selection, toolbar, input, painting |
| `src/annot.rs` | Each drawing tool: how it renders and how it is hit-tested |
| `src/capture.rs` | Grabbing the screen, finding windows and monitors |
| `src/gfx.rs` | Direct2D/DirectWrite/WIC setup, PNG/JPEG export, clipboard |
| `src/pin.rs` | Pinned screenshot windows |
| `src/settings.rs` | Settings file, Settings window, start with Windows |
| `src/countdown.rs` | The delayed-capture countdown |
| `src/dialogs.rs` | Save, colour and folder dialogs |
| `installer/` | Inno Setup script |

## Releasing

Bump the version in `Cargo.toml`, `assets/app.rc` (both `FILEVERSION` lines and the strings) and `assets/app.manifest`, then tag. The release build stops if any of them, or the tag, disagree.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

GitHub Actions builds the installer and the portable exe and attaches them to a **draft** release. Review it and publish it from the Releases page.

## License

Free for personal and other noncommercial use under the [PolyForm Noncommercial License 1.0.0](LICENSE). You may modify it and share your changes, but you must keep the credit to the author (the `Required Notice` line in the license), and you may not use it to make money. For commercial use, contact [@TimWJT](https://github.com/TimWJT) for a commercial license.
