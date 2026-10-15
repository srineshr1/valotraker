# valotraker

A small Valorant lobby overlay. It shows rank, RR, recent results, and K/D for the players in your agent select and live match, then gets out of the way.

The window is drawn by Windows itself. There is no Electron app and no browser engine, which is the part of Valorant Tracker that sits in memory.

Valorant has to be in **Borderless** or **Windowed**. A normal window cannot draw over Exclusive Fullscreen.

## Run

Start `target\release\valotraker.exe`. It has no console window. A tray icon sits next to the clock.

To rebuild on this PC, put MinGW and the user Cargo ahead of other compilers, then build:

```
$env:PATH = "$env:USERPROFILE\.local\w64devkit\bin;$env:USERPROFILE\.cargo\bin;" + $env:PATH
cargo build --release
```

Riot Client needs to be open on this PC. The overlay signs in through the client's local lockfile, then reads the lobby you are already in. It does not read game memory and it does not inject anything into Valorant.

While you are in menus the window hides. It comes back when agent select starts.

`cargo run --release -- --demo` shows a sample lobby without Valorant.

## Keys

| Keys | Action |
| --- | --- |
| Ctrl+Shift+F8 | Show or hide |
| Ctrl+Shift+F7 | Drag the panel. Click-through turns off for 20 seconds |
| Ctrl+Shift+Q | Quit |

The tray icon has the same actions. Right-click it to quit.

Position is saved in `%LOCALAPPDATA%\valotraker\config.json`. Player stats are cached there too, so a rematch does not refetch everyone.

## What a row means

The column names sit above the numbers: `K/D`, `HS%`, `RANK`, `RR`, and `W/L`.

`W/L` is the last few competitive games, oldest on the left. Green gained RR, red lost it, and a dull square was even. K/D and HS% come from up to four recent competitive games. `PLC` means placements, `UR` means no rank this act, `Pick` means they have not chosen an agent yet.

Incognito players in your own match still resolve, because the client already has their id.

## Notes

This uses the same unofficial local client and player-data endpoints other lobby trackers use. A Valorant patch can change them. Tokens stay in memory and are not written to disk.

Log: `%LOCALAPPDATA%\valotraker\valotraker.log`
