# scr8

**English** | [Українська](README.uk.md) | [Русский](README.ru.md)

Instant screenshots of a chosen screen area with a hotkey. For Windows and
macOS.

You set up a **bind** once: a screen area, a folder and a key combination.
Then, in any app, press the combination and a screenshot of that area is
saved to the folder right away. No windows, no sounds. Ten presses in a
second give ten screenshots; holding the combination gives one.

## Download

| System | File |
| --- | --- |
| Windows 10 and 11 | [**scr8-windows.exe**](https://github.com/tsybtw/scr8/releases/latest/download/scr8-windows.exe) |
| macOS 11 or newer (Intel and Apple M1 or newer) | [**scr8-macos.dmg**](https://github.com/tsybtw/scr8/releases/latest/download/scr8-macos.dmg) |

## Installing on Windows

1. Download `scr8-windows.exe` and put it in any folder you like. There is
   nothing to install; this file is the whole app. If your browser warns
   that the file isn't commonly downloaded, choose **Keep**.
2. Run it. The first time, Windows may show "Windows protected your PC".
   Click **More info** → **Run anyway**. This happens with apps that don't
   have a paid digital signature.

If you move the file to another folder later, just run it again from the
new place.

## Installing on macOS

1. Open `scr8-macos.dmg` and drag **scr8** into **Applications**.
2. Start scr8 from Applications. macOS blocks the first launch because the
   app isn't from the App Store:
   - **macOS 14 and older:** right-click scr8 → **Open** → **Open**.
   - **macOS 15 and newer:** close the warning, open **System Settings →
     Privacy & Security**, scroll down, click **Open Anyway** next to the
     message about scr8 and confirm with your password.

   This is needed only once.
3. Allow screen recording. macOS asks for it on the first launch. Turn on
   **scr8** in **System Settings → Privacy & Security → Screen Recording**
   and restart scr8 (macOS offers to do it). Without this permission macOS
   doesn't let any app take screenshots.

On a Mac, scr8 has no Dock icon. It lives in the **menu bar** at the top of
the screen: the icon with four frame corners.

<details>
<summary>macOS says scr8 "is damaged and can't be opened"</summary>

Open the **Terminal** app, paste this command, press Enter and start scr8
again:

```
xattr -dr com.apple.quarantine /Applications/scr8.app
```

</details>

## How to use

The scr8 window opens by itself on the first launch. Later you can open it
from the scr8 icon: on Windows it's in the tray at the bottom right (it may
hide under the **^** arrow), on a Mac it's in the menu bar (choose
**Open scr8**).

**Create a bind:**

1. Click **New bind**. The screen freezes.
2. Drag with the mouse to select the area. Move it by dragging the middle,
   resize it by dragging an edge or a corner. Press **Enter** (or
   double-click the area) to save. **Esc** cancels.
3. Choose the folder to save screenshots to.
4. Press the key combination, e.g. **Ctrl + Alt + 1** (on a Mac, e.g.
   **Cmd + Shift + 1**). It needs at least one modifier key (Ctrl, Alt,
   Shift, Win, or Cmd and Option on a Mac); F1–F20 also work on their own.
   **Esc** cancels.

Done: a green **active** label appears on the right of the bind. Pressing
the combination now saves a screenshot to the chosen folder.

**Bind buttons:**

| Button | What it does |
| --- | --- |
| **Change** | Set a different key combination |
| **Edit area** | Change the area |
| **➕ Edit hotkey** | Set a second combination that opens area editing for this bind from anywhere, without opening the window (✖ removes it) |
| **Choose…** | Pick a different folder |
| **Open** | Open the screenshots folder |
| Switch next to 🗑 | Turn the bind off and on. An off bind keeps all its settings but takes no screenshots, and its combinations are free for other apps |
| 🗑 | Delete the bind |

Click the bind's name (e.g. "Bind 1") to rename it. Screenshot file names
start with it.

## Good to know

- **Closing the window doesn't quit the app**; it keeps running in the
  background. To exit, click the scr8 icon → **Quit**.
- **scr8 starts with your computer.** Turn this off on the **Advanced** tab →
  **Start with system**.
- **Memory.** By default the settings window stays loaded in the background,
  so it opens instantly. To save memory, untick **Advanced** → **Keep this
  window loaded**: scr8 then uses about 3 times less memory in the
  background, and the window takes a moment longer to open. Screenshots are
  just as fast either way.
- **Starting scr8 again** just opens the window of the copy that's already
  running.
- **If a screenshot couldn't be saved** (e.g. the folder was deleted or a
  USB drive was unplugged), the scr8 icon turns red, Windows shows a
  notification, and the error appears at the bottom of the window. Click ✖
  to dismiss it.
- **A red message on a bind** means the combination is already used by
  another bind or another app. Click **Change** and pick a different one.
- **Quality.** Screenshots are always saved as lossless PNG; the
  compression level on the **Advanced** tab changes only file size and
  save time:
  - **Fast** (default): fast save, medium-sized files.
  - **No compression**: about as fast as Fast, but huge files.
  - **Balanced**: slower save, files slightly smaller than Fast.
  - **Best**: much slower, files only slightly smaller than Balanced.

  The exact numbers depend on your computer and on what's on the screen.
  **Run test** shows them for your computer.

## Updating

Download the new version from the links above.

- **Windows:** quit scr8 (icon → **Quit**) and replace the old file with the
  new one.
- **Mac:** quit scr8 (icon → **Quit**) and drag the new version into
  Applications, replacing the old one. If screenshots stop working after
  the update, open **Screen Recording** in System Settings, remove scr8 from
  the list with the **−** button, add it again and restart scr8.

## Uninstalling

1. On the **Advanced** tab, untick **Start with system**.
2. Quit scr8: icon → **Quit**.
3. Delete `scr8-windows.exe` (on a Mac, drag scr8 from Applications to the
   Trash).

Settings are kept in a `scr8` folder, which you can delete too. The exact
path is shown at the bottom of the **Advanced** tab:

- Windows: `C:\Users\<your name>\AppData\Roaming\scr8` (you can paste
  `%APPDATA%\scr8` into the File Explorer address bar to open it)
- Mac: `~/Library/Application Support/scr8`
