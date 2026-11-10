# Joining a vid call

You were sent an **invite link** that looks like:

```
https://relay.example.com:4443/<room>?jwt=<a long code>
```

Keep it to yourself: anyone holding it can join that room until it expires.

## macOS (Apple Silicon or Intel)

1. Unzip `vid-macos-…zip` and move **Vdr.app** to Applications.
2. The app is not signed by Apple yet, so the first time, **right-click Vdr.app
   and choose Open**, then Open again. (If macOS says it is "damaged", run
   `xattr -dr com.apple.quarantine /Applications/Vdr.app` in Terminal once.)
3. Allow camera and microphone when asked.
4. To share your screen, macOS asks once for **Screen Recording**: turn Vdr on
   in System Settings › Privacy & Security › Screen & System Audio Recording,
   then let it quit and reopen. Each new build of Vdr may ask again.

## Linux

1. Unpack `vid-linux-…tar.gz` and run `./vdr` from the unpacked folder.
2. It needs `libasound2`, `libvulkan1` with a Vulkan driver (`mesa-vulkan-drivers`
   on most machines), and an X11 or Wayland desktop.
3. Screen sharing is offered on X11 desktops only for now; on Wayland the
   button does not appear.

## Join

**Paste the invite link into the field at the top and press Connect.**

- The buttons at the bottom left turn your microphone and camera on and off;
  the small ▾ beside each picks a different device.
- The screen button next to them lists your displays and windows; pick one to
  share it. Click the button again, or Stop on your screen's card, to stop.
- **Leave** is at the bottom right.

Headphones avoid echo if you are in the same room as another participant.

## After a call: send the log

The app writes a log every time it runs, which says which microphone and
speakers it used and how well its echo canceller is doing. If a call had echo
or anything else odd, send the file from that run:

- macOS: `~/Library/Application Support/vdr/vdr.log`
- Linux: `~/.local/share/vdr/vdr.log`

`vdr.prev.log` beside it is the run before, in case you have started the app
again since.

## For the test call (if you were asked to measure)

Start it from a terminal instead, so it writes measurements to a file:

```sh
# macOS
/Applications/Vdr.app/Contents/MacOS/vdr --relay "<invite link>" --stats ~/s7-<your-name>.jsonl
# Linux
./vdr --relay "<invite link>" --stats ~/s7-<your-name>.jsonl
```

Check your clock is in sync first (macOS: `sntp -t 1 time.apple.com`; Linux:
`chronyc tracking` or `timedatectl`), and send the `.jsonl` file back after
the call.
