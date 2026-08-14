# P5136 XUN 시험용 사이드카

This is a 32-bit, version-pinned experimental compatibility layer for an
independent XUN driving backport. It builds both a startup `dinput8.dll` proxy and a standalone
`p5136-xun.dll` that can be loaded into an already running client with
`p5136-xun-attach.exe`. Both paths verify the exact unpacked P5136 executable,
expose the same status ABI, and write `p5136-xun-sidecar.log` beside the DLL.

With `enabled=1`, ABI v11 installs hooks on the exact P5136 tachometer factory,
V1/X tachometer allocators, V1 display update, four-stat converter, drive-event
handler, and main physics tick. The hooks preserve the original calls, keep XUN
charger state in a side table keyed by `GoPlayKart*`, and log charger
transitions. Opt-in consumers reproduce all six recovered speed-mode formulas:
speed-derived ordinary boost-gauge charging, drift boost-gauge charging,
wall/booster Exceed-gauge additions, and the two collision-response selections.
They do not change vehicle acceleration or top speed. Charger activation is
not mapped onto P5136's ordinary Exceed state or resource. The importer derives
P5136's `ExceedWaveType` from modern `defaultExceedType`, preserving the stock
ordinary Exceed wave independently of charger state.
`XunGenTacho` is resolved to the P5136 V1-layout implementation while the
imported XUN BML remains the visible skin. A separate native flat-gauge
controller clips `instChargerGauge` continuously from booster-use progress and
active-time remaining. The three `blinkRoad` nodes retain their modern road
state role and are not used as charger steps. The display hook applies the
modern body/default-part conversion without changing KartSpec physics.
Installation checks sixteen complete instruction boundaries, suspends and
inspects other client threads, patches the sites together, and remains inert
if any check fails. The connector writes `p5136-xun-session.ini` beside the
game executable, and a background DLL thread connects to the server's private
TCP endpoint at configured base port +3. The server selects the profile from
the same kart ID used to build KartSpec physics. S/B/L baseline XUN types
2/3/4 are enabled. Type 1 uses the server-side starting-item path and needs no
DLL physics consumer; special types 5-10 remain fail-closed.

This is not complete XUN driving support yet. It implements the recovered
booster-count, activation, duration, expiry, all six speed-mode consumers,
continuous dashboard fill, exact four-stat display conversion, and an
independent charger aura without growing P5136's `GoPlayKart`. The aura uses a
process-lifetime side-table renderer object built on P5136's compatible
`ReCrashEffect` ABI; its attached scene is replaced with the imported
`effect/charger/카트바디차저발동` resource. Activation enables the root and
starts every child emitter at the current race time; expiry stops the children
before disabling the root. Each imported billboard receives P5136's native
alpha property and `-1000.0` render depth, matching the later charger's scene
initialization. Item-profile starting items are granted by the
server from exact `defaultExceedType=1` metadata. XUN-only animations and
lead-charge packet/state behavior remain pending.

The accepted executable is fixed to:

- PE32/x86 timestamp `0x6407DD11`;
- image size `0x0141A000`;
- entry RVA `0x00BE4D56`;
- SHA-256
  `FD9444C057090C3BB524AF03BFF5EC995620FBB951B9A823D2CD4E9B0494956F`.

Any mismatch remains inert and is logged. No client-derived bytes, RHO
payload, IDB, or decompiler output are included.

## Build

```powershell
cmake -S native\p5136-xun-sidecar `
  -B target\p5136-finish-kart-abilities\xun-sidecar-win32 `
  -G "Visual Studio 17 2022" -A Win32 `
  -DP5136_TEST_EXECUTABLE=C:\Nexon\KartRider_5136\KartRiderU.exe
cmake --build target\p5136-finish-kart-abilities\xun-sidecar-win32 --config Release
ctest --test-dir target\p5136-finish-kart-abilities\xun-sidecar-win32 -C Release --output-on-failure
cmake --install target\p5136-finish-kart-abilities\xun-sidecar-win32 `
  --config Release `
  --prefix target\p5136-finish-kart-abilities\release\xun
```

For attach mode, start the exact unpacked client and run
`p5136-xun-attach.exe` with the path to `p5136-xun.dll`. The P5136 GUI defaults
both paths to files beside `p5136.exe`, but also lets the user select each file
independently. If more than one KartRider
process exists, pass the target PID explicitly. The helper refuses every
process whose executable hash or PE identity differs from the pinned build and
must run at the same elevation as the client.

The release keeps the optional startup proxy under `startup-proxy/dinput8.dll`.
Copy it beside `KartRider.exe` only when deliberately using startup injection;
do not place that proxy beside the P5136 launcher.

Successful diagnostic output reports
`status=6 hooks=xun-tacho+physics-state+six-consumers+charger-visual`.
For the live V1 validation pass, set `enabled=1`, restart the exact unpacked
client, attach `p5136-xun.dll`, drive a V1 kart, and use four ordinary
boosters. `p5136-xun-sidecar.log` records sparse physics-tick samples, every
accepted kind-3/kind-4 drive event, the client
counter transition at `GoPlayKart+0xDD4`, and server-selected charger
transitions. Protocol-v2's 52-byte profile frame supplies the S/B/L booster
count, duration, and four default part types used by display conversion.
The consumers apply raw `chargeBoostBySpeedAdded=350`,
`driftGaugeFactor=2`, wall
Exceed gain `+0.09`, booster Exceed gain `+0.03`, anti-collision balance `0.8`,
and the fixed XUN wall-collision response multiplier `100`. A new profile
resets all side-table state, and only the first local kart pointer that emits a
qualified booster-use event is allowed to consume the selected profile. A
successful XUN asset import also installs `effect/charger` (one `.1s` and three
textures). The log records every `charger visual` renderer-object
create/start/stop transition; a missing imported scene stays fail-closed with
no activation.

Hook threads never open, write, or flush the log file. They copy bounded
messages into a fixed 256-entry queue and signal a below-normal-priority writer
thread. The writer owns the file handle and flushes at most once per second; a
queue overflow is reported as a dropped-entry summary instead of blocking the
physics tick.

The same update hook samples P5136's stock `instAccel` flat-gauge controller
after each original update. Only 5-percent fill boundaries or Exceed state
changes are queued as `tachometer Exceed gauge` records, so a missing visual
marker can be diagnosed independently of the underlying gauge value without
turning the renderer path into synchronous logging.

The importer deliberately treats Exceed and charger as separate dashboard
surfaces. It exposes `exceedFeatures`, converts `instAccel` to the textured
legacy Panel contract consumed by P5136's stock Exceed controller, and hides
the newer-only always-on/full-charge overlays. `chargerFeatures` contains a
separate `instCharger` window and continuous `instChargerGauge`; the sidecar
interpolates each booster increment across ten update ticks, matching the
later client. The server also projects the newer `defaultExceedType` and four
default part types through the sidecar profile; it does not append a newer
KartSpec tail that the 5136 client cannot decode.

The sidecar does not copy the latest client's larger tachometer C++ object or
vtable into P5136. Instead, it keeps the proven P5136 V1 object layout, loads
the actual `gui/tachometer/xun` resource, and stores XUN-only charger state in
the DLL side table. This avoids corrupting adjacent P5136 memory while still
providing an XUN skin and a state-driven charger indication.
