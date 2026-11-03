# AgentsPads Technical Documentation

[Back to product overview](../README.en.md) · [简体中文](technical.zh-CN.md) | **English**

Implementation details, diagnostic logs, update behavior, and project structure for readers exploring the internals or troubleshooting issues.

[Implementation and constraints](#technical-implementation-and-constraints) · [Log locations](#log-locations) · [Updates](#automatic-checks-and-confirmed-updates) · [Project structure](#build)

---

## Technical Implementation and Constraints

| Module | Current Implementation | Constraints & Safety Boundaries |
| :--- | :--- | :--- |
| **Architecture** | Android phone client built with Flutter; desktop daemon built in pure Rust for Windows and macOS | Strictly no Electron, Python runtime, or Flutter desktop shells, ensuring minimal resource footprint; the desktop pairing window repaints only when adapters, permissions, the pairing code, or update status change, or on user input, and presents no frames while idle |
| **Android Native Bridge** | Lightweight native Kotlin module: binds to physical Wi-Fi only for same-subnet targets, manages OkHttp WebSocket connections, monitors anonymous recording state, inspects the IME subtype, and forwards an IME Backspace to the desktop when the input box is empty | UI rendering, reactive state, and gestures remain in Flutter; never records audio, requests no microphone permissions |
| **Transport & Protocol** | Phone connects to desktop TCP `9618` via plain LAN WebSockets; messages use a fixed JSON schema. On every connection the desktop first sends a random challenge, and the phone must answer with an HMAC-SHA256 computed from the pairing key (or, for a first manual pairing, submit the 4-digit pairing code); connections that do not pass within 10 seconds are closed | No cloud relays, no user accounts, no external internet dependency; **authentication only blocks unauthorized devices from connecting; traffic is not encrypted. Anyone who can capture packets on the same network can read the text you send, and anyone who can intercept traffic can tamper with input. Use on untrusted networks (public Wi-Fi, shared offices, etc.) is not recommended; do not expose the port to the public internet** |
| **Desktop Listener** | Daemon listens on `0.0.0.0:9618`, never binding exclusively to a single network interface | Ethernet, Wi-Fi, and hotspot addresses can connect when the endpoint is unicast-reachable and local firewall rules allow TCP 9618 |
| **Pairing Mechanism** | Desktop displays a system DPI-scaled (~260pt) crisp QR code alongside a copyable `IP:port`; active physical, tunnel, and virtual-network adapter IPv4 addresses are switchable in the adapter list beside the QR. The QR carries the pairing key; manual address entry requires the 4-digit one-time pairing code shown in the pairing window (invalid once the window is hidden, replaced after each use, locked after 5 wrong attempts in total), after which the desktop hands out the pairing key | Inactive adapters and adapters without a candidate IPv4 address are filtered; common container/VM interfaces that can be clearly identified as machine-local are also excluded; phone also supports manual entry and persistent candidate IP pools. During manual pairing, both the code and the key handed out afterwards travel in plaintext, so they can still leak if someone is eavesdropping at that moment; "Reset pairing key" on the pairing window's Settings tab revokes every paired phone |
| **Multi-Device Management** | Each computer maintains an independent WebSocket connection with its own reconnect logic; the phone uniformly sends to all checked, activated online targets; the target strip sits below the input by default and can be moved next to "Connected" in the top bar | Input box on phone clears immediately once any checked target succeeds; does not block on ACK from every machine. Connections and retries stop while the phone app is hidden and resume automatically when it becomes visible |
| **Text Injection Pipeline** | Text finishes composition on the phone keyboard; desktop writes plain text to clipboard, then injects genuine `Ctrl+V` / `Cmd+V`. If Automatic Enter is active and paste succeeds, system waits 80ms for the target app to digest text before sending Enter | Desktop receives standard paste shortcuts rather than simulated keystrokes for `v`; secure fields or apps disabling paste may reject input. Normal text sync and manual Enter bypass this 80ms delay |
| **Voice Detection Logic** | Focus must be accompanied by active Android recording or an IME subtype declaring `voice`; cursor must be at end of text, with no selection and finished composition | Relies on low-level source evidence rather than text heuristics, preventing typing or paste alone from triggering auto-send and supporting standard voice IMEs without whitelists. Configurable stability delay (0 / 0.5s / 1s / 1.5s, default 0.5s) allows AI restructuring time. Recording detection requires Android 7+; Android 5/6 relies on `voice` subtype. Concurrent unrelated recording is the remaining theoretical edge case |
| **Shortcut Injection** | Phone sends key + modifiers; Windows calls `SendInput`, macOS calls `CGEvent` to inject genuine keydown / keyup events. When the input box is focused and empty, a soft-keyboard or hardware Backspace follows the same path and injects `Backspace` without using the shortcut row; when the input box is empty the Send button switches to a return-arrow icon and tapping it injects `Enter` through the same path | One unified global shortcut mapping; no per-application configuration profiles. Empty-box Backspace is forwarded only while the phone input still has focus and contains no text; leftover draft text is edited on the phone only. Empty-box Enter shares that same empty-box check with Backspace; as soon as the box holds text the Send button reverts to sending text |
| **Undo Mechanism** | Phone instructs target computers to inject `Ctrl+Z` on Windows or `Cmd+Z` on macOS, restoring the last sent draft back into the phone input | Undo efficacy depends on target application undo history; terminals and CLI tools may ignore or reinterpret shortcut; executed commands and terminal output cannot be rolled back |
| **Pointer & Cursor Control** | Phone transmits relative deltas, button bitmaps, and logical-pixel scroll deltas; a seven-tier slider offers 60/120/240/320/480/560/960Hz, defaulting to the highest tier at or below twice the display's **peak supported refresh rate** (120Hz → 240Hz; 230Hz → 320Hz; detection failure defaults to 60Hz), while preserving manual choices; Android window requests peak refresh to prevent frame throttling; separate Windows / macOS settings adjust pointer speed (×1–×7 on both, default Windows ×6 / macOS ×3) and wheel speed (Windows ×1–×7, default ×1; macOS ×4–×28 in steps of ×4, default ×16), with independent scroll direction settings (macOS reversed by default); injected via `SendInput` / `CGEvent`; macOS consecutive taps write the system click count (about 500ms and 5pt, up to triple-click; a drag breaks the sequence) | Strictly no fabricated path interpolation. When multiple deltas arrive within a single display refresh, cursor may appear to jump — this is delayed motion drawn at once, not misplacement; round trips maintain strict zero-drift precision. Trackpad / trackball / pointing stick share speed & wheel side configs; not physical HID hardware, not intended for esports or multi-display absolute mapping |
| **Local State Persistence** | Android stores device lists, selection states, shortcuts, themes, voice delays, input heights, pointer modes/sizes/speeds/rates, wheel speed/side/direction, device-strip placement, landscape layout, force-landscape, and long-press haptic via `SharedPreferences` | Zero remote account synchronization; 100% of configuration and historical data remains strictly on the local device |
| **System Permissions** | Android requires network access and camera permission for QR scanning; voice detection requires no microphone permission; macOS requires "Accessibility" under System Settings for key and cursor injection | macOS Accessibility TCC and code signing are separate mechanisms; Windows Firewall must allow LAN TCP 9618 traffic |
| **Desktop Launch at Login** | The Settings tab of the desktop pairing window provides a default-off launch-at-login toggle; Windows creates or removes an entry in the Startup folder, while packaged macOS apps create or remove a LaunchAgent under `~/Library/LaunchAgents/` | The local login entry is written only after explicit user opt-in, requires no network access or elevation, and is removed when the toggle is disabled |
| **Windows Administrator Mode** | "Launch as administrator" on the Settings tab is off by default; enabling it asks for UAC approval once and registers a scheduled task that runs with highest privileges, through which every later manual launch silently restarts as administrator, while launch at login is handled by a second task with a logon trigger that starts directly as administrator | Windows only; takes effect only after explicit opt-in and UAC approval, and exists to inject input into administrator windows such as Task Manager; the next launch after turning it off runs with normal privileges; the administrator instance keeps its pairing key separate from the normal instance, in a directory only Administrators/SYSTEM can access, so ordinary processes cannot read it and phones must re-scan after switching modes; if the scheduled task stops working (for example, the EXE was moved), AgentsPads runs with normal privileges and turns the toggle off so it can be re-registered; the administrator instance stores its theme, guide flag, and diagnostic logs in `%ProgramFiles%\AgentsPads\state` (permissions inherited from the program directory, readable by ordinary users); in administrator mode, launch at login is a second highest-privilege scheduled task with a logon trigger (about a 30-second delay), switched on and off by the administrator instance with `schtasks /Change` without writing to the user profile; turning administrator mode on removes the old Startup-folder entry, and turning it off returns to the normal setting from before; later launches start as administrator only when a marker in that program directory says so |

---

## Log Locations

Diagnostic logs are off by default and apply only to the current run. When you hit connection issues or unexpected behavior, turn on "Diagnostic logs" on the pairing window's Settings tab (enabling it clears old logs), reproduce the problem, then click "Open folder" or choose "Open Logs" from the tray menu; "Clear" deletes existing logs at any time. Logging turns itself off when AgentsPads restarts. Log locations:

- **Windows**: `%APPDATA%\AgentsPads\logs\`; in administrator mode, `%ProgramFiles%\AgentsPads\state\logs\`
- **macOS**: `~/Library/Logs/AgentsPads/`

---

## Automatic Checks and Confirmed Updates

AgentsPads uses a lightweight cross-platform update channel. Every stable Release includes a small manifest containing the version, download URLs, and SHA-256 values. Clients read the GitHub Release asset first and fall back to two CDN mirrors, avoiding dependence on GitHub REST API quotas. All three clients share the same check rhythm; downloads and installation always require user confirmation, and the app never updates itself unattended:

- **Automatic check schedule**:
  - The desktop and Android apps perform one background check after every launch;
  - While an app remains running, it checks again every 24 hours from startup. Restarting starts a new process and therefore performs another launch check;
  - Checks use an in-process HTTPS client and fall back through the CDN mirrors when the primary source is unavailable; they never open CMD, a terminal, or an unattended download;
  - Background checks and automatic notifications do not open dialogs on their own: desktop shows a clickable update entry beside the version number in the window's top bar only when a newer release is found, and keeps the check status on the About tab. On Android, a newer release appears as a green dot and an "Update" badge beside the version number at the bottom of the home screen; the confirmation dialog opens only after you tap it. The manual "Check for Updates" command reports checking, up-to-date, or failure.
- **Visible version status**:
  - The desktop pairing window's top bar always shows the running version beside the listening port; the About tab shows the latest check result and offers a manual "Check for Updates";
  - When a newer release is found, an "**Update to vX.Y.Z**" button appears beside the top-bar version and on the About tab. Download and replacement begin only after the user clicks it.
- **Windows (In-Process Download and Self-Replacement)**:
  - After the user clicks "Update to vX.Y.Z," AgentsPads downloads the new `agentspads-windows-x64.exe` through its in-process HTTPS client and reports progress in the UI; it does not invoke CMD, an updater script, or the system `curl`;
  - Once the download completes, AgentsPads verifies its SHA-256 value before renaming the running EXE to `.old`, placing the new executable at the original path, and relaunching. A failed verification or replacement keeps the previous version and surfaces the reason in the UI;
  - On startup, AgentsPads removes `.old` / `.new` update residue. A legacy `agentpad_updater.bat` left by older releases is removed only when its contents match the former AgentPad updater, avoiding accidental deletion of user files.
- **macOS (Automatic App Bundle Swap)**:
  - After the user clicks "Update to vX.Y.Z," AgentsPads downloads `agentspads-macos-arm64.zip`, verifies its SHA-256 value from the manifest, and only then extracts it;
  - When running from any `.app` bundle in a writable location, AgentsPads renames the old bundle aside, places the new bundle at the original path, and launches it through `open -n`; this is not restricted to `/Applications`;
  - If the bundle cannot be swapped by rename, AgentsPads falls back to an in-place `ditto` copy. Only non-bundle development builds open the extracted directory for manual handling; no DMG mount is required.
- **Android (System Installer Confirmation)**:
  - The bottom of the home screen permanently shows the current version number, following the same background check after launch and every 24 hours;
  - When an automatic check finds a newer release, only a green dot and an "Update" badge light up beside the version number, with no dialog; tapping it opens the release notes, and only after tapping "Download & Update" does AgentsPads download `agentspads.apk` and open the system installer. The application id is `app.agentspads`. An in-place install of that same id, signed with the same key, retains saved devices and preferences. An existing install whose id is still `app.agentpad` is left in place; install the new package separately, and data from the old app does not come along.

---


<a id="build"></a>

## Project Structure

```text
AgentsPads/
├── desktop/                 # Desktop daemon (Pure Rust core: Windows / macOS)
│   └── crates/
│       ├── agentpad/        # System tray, pairing UI window, WebSocket server
│       └── agentpad-input/  # Plain text injection, system keystrokes & cursor engine
├── android/                 # Mobile client (Android application)
│   ├── lib/                 # Flutter UI, state management, gesture recognizers, protocol client
│   └── android/app/src/main/kotlin/
│       └── app/agentspads/  # Physical Wi-Fi binding, WebSocket client, voice evidence native bridge, empty-box Backspace
├── docs/                    # Technical documentation
│   ├── technical.zh-CN.md    # Simplified Chinese
│   └── technical.en.md       # English
├── LICENSE
├── README.md
└── README.en.md
```
