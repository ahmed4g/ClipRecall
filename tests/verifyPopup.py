#!/usr/bin/env python3
"""Phase 4 live acceptance gate. Run inside a live GNOME Wayland session with the
extension loaded (a logout/login is required for GNOME Shell to register the UUID).

    python3 tests/verifyPopup.py

Exit codes: 0 every gate with a threshold passes, 2 nothing can be measured (extension
not loaded, or key injection unavailable), 1 a gate failed.

Gates (plan.md Phase 4):
  A. the keybinding fires and the popup opens at the pointer in < 50 ms
  B. typing performs an instant search (< 25 ms from query to render)
  C. a quick select key selects the row its badge shows (with an empty entry)
  D. rendering is paged: at most 20 rows queried on demand, and the second page is fetched
     when navigation reaches past what was loaded
  E. measure shell main loop responsiveness while the popup holds the grab (proxy for frame drops)
  F. Escape closes the popup, and a click outside it closes it

A gate that fails does not stop the run: every gate is attempted and the summary at the
end is what the exit code reflects. The first live runs stopped at the first failure, so
the measurements that could have been taken after it were lost.

How each gate is measured, and what that measurement can honestly claim:

- The timings and the position come from the extension's own journal lines, because an
  outside process cannot time what the shell did inside its own main loop. The extension
  logs the open latency, the position it used, the pointer it read, the work area, the
  query-to-render time, the close, and the id of the row a key selected. Renders carry a
  history or search label. The journal never needs clipboard content or the search text.
- Keys are injected through /dev/uinput with ydotool, which is a real input device the
  compositor reads. Delivery was confirmed on 2026-11-06: an injected Super press set
  org.gnome.Shell OverviewActive to true, and an injected letter fired a shell keybinding.
  A combo fires only for a binding registered through Main.wm: pointing
  org.gnome.shell.keybindings toggle-overview at <Super>V made an injected Super+V open the
  overview, while the same combo did nothing for a binding added straight to
  global.display. So a popup opening after an injected Super+V is the keybinding firing, not
  a call made from outside.
- Gate E measures shell main-loop responsiveness through D-Bus round trips. Frame-drop
  measurement requires a separate compositor trace.
- The visual highlight cannot be read from outside the shell, so gate C verifies the key ->
  row mapping end to end instead: the digit is pressed on a freshly opened popup (a digit is
  quick select only while the entry is empty), and the row it should select must become the
  clipboard owner. The digit reaches the popup as a text change, not as a key event, because
  Clutter.Text consumes printable keys before the popup's key handler runs (measured live on
  2026-11-06: pressing 1 produced a search for "1" instead of selecting a row).
  The clipboard is set beforehand with a password manager hint copy, which
  the extension rejects as a row but which still changes the clipboard, so a matching
  wl-paste can only come from the popup restoring the selected row.
- Gate D reads the window start and the request count the extension logs. A window that
  starts past the first page can only exist if a second page was fetched, so the paging rule
  is measured live rather than asserted from the unit test. The selection has to be able to
  reach the row that is not loaded yet; clamping it to the loaded rows made the second page
  unreachable, which is what the first live run measured (20 Down presses, one request).
- Key focus is proven indirectly: if the typed characters produced a search, they reached
  the popup rather than the window that had focus before it opened.
- Clipboard reads and writes from an ordinary client do not complete while the popup holds the
  modal grab (measured 2026-11-06: wl-copy and wl-paste hung until the popup closed). That is why
  the gate seeds its rows with the popup closed, and why selecting a row has to close the popup
  before the paste path runs.

Like the Phase 3 gate, this writes rows into the real history database.
"""

import argparse
import json
import re
import shlex
import shutil
import subprocess
import sys
import time

UUID = "clipboard-manager@project.local"
BUS = "org.clipboardmanager.Daemon"
PATH = "/org/clipboardmanager/Daemon"
IFACE = "org.clipboardmanager.Daemon"
PREFIX = "[clipboard-manager]"

ROW_FIELDS = ("id", "primary_mime", "preview", "sort_time", "pinned", "has_thumbnail", "source_app")

# evdev key codes, as ydotool expects them. Verified against
# /usr/include/linux/input-event-codes.h on 2026-11-06: KEY_V is 47, not 23 (23 is KEY_I),
# and the first run of this gate injected Super+I because of that.
KEY_ESCAPE = 1
KEY_1 = 2
KEY_LEFTCTRL = 29
KEY_RIGHTCTRL = 97
KEY_DOWN = 108
KEY_LEFTMETA = 125
KEY_V = 47
QUICK_SELECT_MAX = 9

# The popup size and the render cap are read from the source so the gate cannot drift from it.
def popup_size():
    src = open("extension/popup.js", encoding="utf-8").read()
    w = re.search(r"const POPUP_WIDTH = (\d+);", src)
    h = re.search(r"const POPUP_HEIGHT = (\d+);", src)
    if not w or not h:
        raise RuntimeError("could not read POPUP_WIDTH/POPUP_HEIGHT from extension/popup.js")
    return int(w.group(1)), int(h.group(1))


def max_visible():
    src = open("extension/popupLogic.js", encoding="utf-8").read()
    m = re.search(r"export const MAX_VISIBLE = (\d+);", src)
    if not m:
        raise RuntimeError("could not read MAX_VISIBLE from extension/popupLogic.js")
    return int(m.group(1))


def call(method, signature, *args):
    cmd = ["busctl", "--user", "call", BUS, PATH, IFACE, method]
    if signature:
        cmd.append(signature)
        cmd.extend(str(a) for a in args)
    res = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
    if res.returncode != 0:
        raise RuntimeError(f"{method} failed: {res.stderr.strip() or res.stdout.strip()}")
    return res.stdout.strip()


def rows_of(method, signature, *args):
    out = call(method, signature, *args)
    # busctl prints the signature, the element count, then the values in order.
    tokens = shlex.split(out)
    if not tokens or tokens[0] != "a(ssstbbs)":
        raise RuntimeError(f"unexpected {method} reply: {out}")
    count = int(tokens[1])
    values = tokens[2:]
    if len(values) != count * len(ROW_FIELDS):
        raise RuntimeError(f"{method} reply has {len(values)} values for {count} rows")
    rows = []
    for i in range(count):
        row = dict(zip(ROW_FIELDS, values[i * len(ROW_FIELDS):(i + 1) * len(ROW_FIELDS)]))
        row["sort_time"] = int(row["sort_time"])
        row["pinned"] = row["pinned"] == "true"
        row["has_thumbnail"] = row["has_thumbnail"] == "true"
        rows.append(row)
    return rows


def entry_count():
    m = re.search(r"sutb\s+(\S+)\s+(\d+)\s+(\d+)\s+(\S+)", call("Health", ""))
    if not m:
        raise RuntimeError("unexpected Health reply")
    return int(m.group(2))


def ydotool(*args):
    # Consecutive Control chords can activate global double-Control shortcuts.
    if args[0] == "key" and any(event.partition(":")[0] in (str(KEY_LEFTCTRL), str(KEY_RIGHTCTRL))
                                for event in args[1:]):
        raise RuntimeError("Live test input cannot send Control keys. Use the app's Paste menu.")
    res = subprocess.run(["ydotool", *args], capture_output=True, text=True, timeout=10)
    if res.returncode != 0:
        raise RuntimeError(f"ydotool {args} failed: {res.stderr.strip() or res.stdout.strip()}")


def press(code):
    ydotool("key", f"{code}:1", f"{code}:0")


def super_v():
    ydotool("key", f"{KEY_LEFTMETA}:1", f"{KEY_V}:1", f"{KEY_V}:0", f"{KEY_LEFTMETA}:0")


def wl_copy(text, sensitive=False):
    # -n so the stored payload has no trailing newline, which keeps the comparison exact.
    cmd = ["wl-copy", "-n"]
    if sensitive:
        # --sensitive announces the password manager hint alongside text/plain, so the
        # extension rejects the copy as a row while the clipboard still changes. That is what
        # makes the quick select check end to end: the clipboard can only change because the
        # popup restored a row.
        cmd.append("--sensitive")
    cmd.append(text)
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=5)


def submit_row(text, app="gate-seed"):
    """Write a row straight through the bus, so gate D can seed more rows than one page."""
    data = text.encode()
    out = call("SubmitItem", "a{say}s", 1, "text/plain", len(data), *[str(b) for b in data], app)
    return shlex.split(out)[-1].strip('"')


def wl_paste():
    # A clipboard read from an ordinary client does not complete while the popup holds the modal
    # grab, so a timeout here is a measurement, not a crash: it means the grab was never released.
    try:
        res = subprocess.run(["wl-paste", "--no-newline"], capture_output=True, check=True, timeout=5)
    except subprocess.TimeoutExpired:
        return None
    return res.stdout


def extension_loaded():
    res = subprocess.run(["gnome-extensions", "info", UUID], capture_output=True, text=True, timeout=5)
    return res.returncode == 0 and "State: ACTIVE" in res.stdout


def journal(since):
    """Extension journal lines as (unix microseconds, message), in the order the journal has them.

    The field name for the timestamp is `microsecond` on some systemd versions and only
    `__REALTIME_TIMESTAMP` on others (measured on this machine on 2026-11-06), so read either.
    """
    seconds, micros = divmod(since, 1_000_000)
    out = subprocess.run(
        ["journalctl", "--user", "-b", "0", "--since", f"@{seconds}.{micros:06d}",
         "--output=json", "--no-pager"],
        capture_output=True, text=True, check=True, timeout=10).stdout
    lines = []
    for raw in out.splitlines():
        try:
            rec = json.loads(raw)
        except json.JSONDecodeError:
            continue
        msg = rec.get("MESSAGE", "")
        if isinstance(msg, str) and PREFIX in msg:
            stamp = rec.get("__REALTIME_TIMESTAMP") or rec.get("microsecond")
            if stamp is not None and int(stamp) >= since:
                lines.append((int(stamp), msg))
    return lines


def wait_line(since, pattern, timeout=3.0):
    """First journal line matching `pattern` that appeared after `since`."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        for stamp, msg in journal(since):
            if re.search(pattern, msg):
                return stamp, msg
        time.sleep(0.1)
    return None, None


def clamp(x, y, w, h, work):
    """The same rule as extension/popupLogic.js clampToWorkArea, checked against the log."""
    max_x = max(work[0], work[0] + work[2] - w)
    max_y = max(work[1], work[1] + work[3] - h)
    return min(max(x, work[0]), max_x), min(max(y, work[1]), max_y)


# The extension logs the window it rendered and how many requests produced it, which is what
# makes the paging rule measurable from outside the shell.
RENDER_LINE = r"(\d+) rows rendered in ([\d.]+) ms window=(\d+) requests=(\d+)"
HISTORY_RENDER_LINE = RENDER_LINE + r" kind=history$"
SEARCH_RENDER_LINE = RENDER_LINE + r" kind=search$"

# The open line carries the latency, the position the popup used, the pointer it read and the
# work area, so the position claim can be checked against the clamp rule rather than trusted.
OPEN_LINE = (r"popup open ([\d.]+) ms at \((-?\d+), (-?\d+)\) "
             r"pointer=\((-?\d+), (-?\d+)\) work=\((-?\d+), (-?\d+), (\d+), (\d+)\)")


def parse_render(msg):
    m = re.search(RENDER_LINE, msg)
    if not m:
        raise RuntimeError(f"unexpected render line: {msg}")
    return int(m.group(1)), float(m.group(2)), int(m.group(3)), int(m.group(4))


def shell_round_trip():
    started = time.monotonic()
    subprocess.run(["busctl", "--user", "get-property", "org.gnome.Shell",
                    "/org/gnome/Shell", "org.gnome.Shell", "Mode"],
                   capture_output=True, text=True, check=True, timeout=10)
    return (time.monotonic() - started) * 1000


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--round-trip-samples", type=int, default=15, help="samples for gate E")
    args = ap.parse_args()
    if args.round_trip_samples < 1:
        ap.error("--round-trip-samples must be positive")

    print("Phase 4 live acceptance gate")
    print("============================\n")

    if not extension_loaded():
        print(f"SKIP: {UUID} is not loaded by GNOME Shell in this session.")
        print("GNOME Shell registers a newly installed UUID only after a logout/login.")
        return 2
    if not shutil.which("ydotool"):
        print("SKIP: ydotool is not installed, so no key can be injected and the keybinding "
              "cannot be fired from a script.")
        return 2
    try:
        call("Health", "")
    except RuntimeError as err:
        print(f"daemon not reachable ({err}); starting the user service")
        subprocess.run(["systemctl", "--user", "start", "clipboard-manager-daemon.service"],
                       capture_output=True, text=True, timeout=10)
        time.sleep(1)
        call("Health", "")

    width, height = popup_size()
    print(f"popup size from extension/popup.js: {width} x {height}\n")

    results = []

    def record(letter, ok, detail):
        # ok is True or False, or None for a measurement that has no threshold to check.
        results.append((letter, ok, detail))
        verdict = "MEASURED" if ok is None else ("PASS" if ok else "FAIL")
        print(f"{letter}: {verdict} - {detail}\n")

    def now():
        return time.time_ns() // 1_000

    # A gate that opens the popup needs it closed first: while a modal grab is held the
    # compositor routes keystrokes to the grabbing actor, so Super+V types into the entry
    # instead of firing the keybinding. A close line proves it was open; silence means it was
    # already closed, which cannot be told apart from outside the shell, so the gates report
    # which one they saw.
    def ensure_closed():
        stamp = now()
        press(KEY_ESCAPE)
        return wait_line(stamp, r"popup closed after", timeout=1.0)[1]

    def open_popup():
        since = now()
        super_v()
        return since, wait_line(since, OPEN_LINE)

    def gate_a():
        # The pointer is moved by a known relative delta first, so the gate knows where the
        # extension read it from (the extension reports the pointer it used).
        ydotool("mousemove", "300", "200")
        since = now()
        super_v()
        stamp, msg = wait_line(since, OPEN_LINE)
        if not msg:
            return False, "the injected Super+V did not open the popup (no journal line); either " \
                          "the keybinding did not fire or the key did not reach the compositor"
        m = re.search(OPEN_LINE, msg)
        open_ms = float(m.group(1))
        pos = (int(m.group(2)), int(m.group(3)))
        pointer = (int(m.group(4)), int(m.group(5)))
        work = tuple(int(m.group(i)) for i in range(6, 10))
        expected = clamp(*pointer, width, height, work)
        print(f"   open in {open_ms:.1f} ms at {pos}, pointer {pointer}, work area {work}")
        print(f"   the journal saw the line {(stamp - since) / 1_000:.0f} ms after input injection started")
        print(f"   position {'matches' if pos == expected else 'does not match'} the clamp rule {expected}")
        return open_ms < 50 and pos == expected, \
            f"keybinding fired and the popup opened at the pointer in {open_ms:.1f} ms (gate < 50 ms)"

    def gate_b():
        # Key focus is proven by the keystrokes reaching the popup: the search line only appears
        # if the popup received them. The rows are seeded with the popup closed, because a
        # data-control clipboard write from an ordinary client does not complete while the popup
        # holds the modal grab.
        token_a, token_b = f"p4-b1-{int(time.time())}", f"p4-b2-{int(time.time())}"
        ensure_closed()
        wl_copy(token_a)
        time.sleep(0.3)
        wl_copy(token_b)
        time.sleep(0.3)
        since, (_, msg) = open_popup()
        if not msg:
            return False, "could not open the popup after seeding the rows"
        _, first = wait_line(since, HISTORY_RENDER_LINE)
        if not first:
            return False, "the popup did not render its first page"
        print(f"   first page: {first.split('] ')[1]}")

        since_type = now()
        ydotool("type", "-d", "1", token_a)
        _, second = wait_line(since_type, SEARCH_RENDER_LINE)
        if not second:
            return False, "typing did not produce a search, so the keystrokes did not reach the popup"
        search_rows, search_ms, _, _ = parse_render(second)
        print(f"   typed {token_a!r}: first search rendered in {search_ms:.1f} ms, {search_rows} rows")
        return search_ms < 25, f"instant search within the gate ({search_ms:.1f} ms < 25 ms)"

    def gate_c():
        # A digit selects a row only while the entry is empty, so the popup is closed and
        # reopened first. The digit arrives as a text change (Clutter.Text consumes printable
        # keys before the popup's key handler), so this gate measures the rule end to end rather
        # than the key handler itself. The clipboard is then set to content the extension rejects
        # (a password manager hint), which changes the clipboard without adding a row, so a
        # matching wl-paste can only come from the popup restoring the row the digit selected.
        # Both copies happen with the popup closed: a data-control clipboard write does not
        # complete while the popup holds the modal grab.
        token_c = f"p4-c-{now()}"
        ensure_closed()
        wl_copy(token_c)
        time.sleep(0.3)
        # History puts pinned items first, so select the badge on the test row rather
        # than assuming that the newest copy always gets badge 1.
        rows = rows_of("GetHistory", "iis", 0, QUICK_SELECT_MAX, "all")
        row_index = next((i for i, row in enumerate(rows) if row["preview"] == token_c), None)
        if row_index is None:
            return False, f"the test copy is outside the first {QUICK_SELECT_MAX} rows or was not captured; " \
                          "use a test history with room for an unpinned quick-select row"
        expected_id = rows[row_index]["id"]
        digit = row_index + 1
        marker = f"p4-c-marker-{now()}"
        wl_copy(marker, sensitive=True)
        time.sleep(0.3)
        if any(row["preview"] == marker for row in rows_of("SearchHistory", "si", marker, 5)):
            return False, "the sensitive copy was stored as a row, so the clipboard change does not " \
                          "prove the popup restored it"

        since, (_, msg) = open_popup()
        if not msg:
            return False, "could not reopen the popup with an empty entry"
        # The quick select path picks a row from what is rendered, so the first page has to have
        # landed before the digit is pressed; pressing it at the moment of open would select
        # nothing and the gate would be measuring a race rather than the rule.
        _, first = wait_line(since, HISTORY_RENDER_LINE)
        if not first:
            return False, "the popup did not render its first page before the digit was pressed"
        print(f"   first page: {first.split('] ')[1]}")

        since_key = now()
        # evdev digit keys 1 through 9 have consecutive codes.
        press(KEY_1 + row_index)
        select_line = r"selected row (\S+) \(index (\d+)\)"
        _, sel = wait_line(since_key, select_line)
        if not sel:
            return False, "the digit did not select a row"
        selected_id, selected_index = re.search(select_line, sel).groups()
        if selected_id != expected_id or int(selected_index) != row_index:
            return False, f"digit {digit} selected {selected_id} at index {selected_index}, expected " \
                          f"{expected_id} at index {row_index}"
        print(f"   digit {digit} selected the expected row {selected_id}")
        if not wait_line(since_key, r"popup closed", timeout=2.0)[1]:
            return False, "selecting a row did not close the popup, so the modal grab was never released"
        print("   the popup closed on select, which released the grab")

        time.sleep(0.5)  # the getContent round trip and set_owner
        pasted = wl_paste()
        if pasted is None:
            return False, "wl-paste timed out, so the clipboard read never completed — the grab was " \
                          "still held"
        if pasted != token_c.encode():
            return False, f"the clipboard bytes after quick select {digit} did not match the test row"
        print(f"   that row became the clipboard owner (the clipboard held {marker!r} before the key)")
        return True, "the quick select key mapped to the row its badge shows, end to end"

    def gate_d():
        cap = max_visible()
        total = entry_count()
        if total <= cap:
            seed = now()
            for i in range(cap + 3 - total):
                if not submit_row(f"p4-d-{seed}-{i}"):
                    return False, "the daemon skipped a paging fixture; check whether history capture is paused"
            total = entry_count()
        if total <= cap:
            return False, f"paging needs more than {cap} history entries, but only {total} are available"
        print(f"   history has {total} entries, cap {cap} rows per page")

        ensure_closed()
        since, (_, msg) = open_popup()
        if not msg:
            return False, "could not open the popup for the paging check"
        _, first = wait_line(since, HISTORY_RENDER_LINE)
        if not first:
            return False, "the popup did not render a page"
        rows1, ms1, start1, requests1 = parse_render(first)
        print(f"   first render: {rows1} rows in {ms1:.1f} ms, window starts at {start1}, "
              f"{requests1} request(s)")
        if rows1 != cap or requests1 != 1 or start1 != 0:
            return False, f"the first page should be one request from offset 0 with {cap} " \
                          f"rows, got {rows1} rows from {start1} in {requests1} request(s)"

        # Walk past the loaded page; the selection reaching row `cap` is what asks for page two.
        since_nav = now()
        for _ in range(cap):
            press(KEY_DOWN)
        _, second = wait_line(since_nav, HISTORY_RENDER_LINE)
        if not second:
            return False, f"navigating to row {cap} did not fetch a second page"
        rows2, ms2, start2, requests2 = parse_render(second)
        print(f"   after {cap} Down presses: {rows2} rows in {ms2:.1f} ms, window starts at "
              f"{start2}, {requests2} request(s)")
        if requests2 != 2 or start2 != cap or not 0 < rows2 <= cap:
            return False, "the second page should add one request and render a nonempty window from the next offset"
        return True, f"paged rendering: at most {cap} rows queried, second page fetched on demand"

    def gate_e():
        # The popup has to be open for this to measure the main loop while it holds the grab.
        ensure_closed()
        _, (_, msg) = open_popup()
        if not msg:
            return False, "could not open the popup to measure the shell while it holds the grab"
        samples = [shell_round_trip() for _ in range(args.round_trip_samples)]
        median = sorted(samples)[len(samples) // 2]
        print(f"   {args.round_trip_samples} shell D-Bus round trips while the popup is open: "
              f"median {median:.1f} ms, worst {max(samples):.1f} ms")
        return None, "shell responsiveness proxy; frame-drop measurement requires a compositor trace"

    def gate_f():
        ensure_closed()
        _, (_, msg_open) = open_popup()
        if not msg_open:
            return False, "could not open the popup for the Escape check"
        since_esc = now()
        press(KEY_ESCAPE)
        _, msg = wait_line(since_esc, r"popup closed after (\d+) ms")
        if not msg:
            return False, "Escape did not close the popup"
        print(f"   Escape closed the popup ({msg.split('] ')[1]})")

        # Open, then move the pointer to a point that is inside the work area but outside the
        # popup rectangle, and click there. The popup is the modal grab actor, so a click
        # anywhere reaches it; what is being checked is that a click outside its own bounds
        # closes it.
        _, (_, msg_open) = open_popup()
        if not msg_open:
            return False, "could not reopen the popup for the outside-click check"
        m = re.search(OPEN_LINE, msg_open)
        px, py, qx, qy, wx, wy, ww, wh = (int(m.group(i)) for i in range(2, 10))
        outside = next(((cx, cy) for cx, cy in
                        ((px - 60, py - 60), (px + width + 60, py - 60),
                         (px - 60, py + height + 60), (px + width + 60, py + height + 60))
                        if wx <= cx < wx + ww and wy <= cy < wy + wh), None)
        if not outside:
            return False, "no point in the work area was outside the popup, so the click could not " \
                          "be placed outside it"
        # Negative deltas need the `--` separator, otherwise ydotool reads them as options.
        ydotool("mousemove", "--", str(outside[0] - qx), str(outside[1] - qy))
        since_click = now()
        ydotool("click", "0xC0")  # left button down and up
        _, closed = wait_line(since_click, r"popup closed after (\d+) ms")
        if not closed:
            return False, "a click outside the popup did not close it"

        # Relative input can be accelerated or clamped. Reopen without moving the pointer
        # and use GNOME Shell's pointer reading to check where the click landed.
        _, (_, probe) = open_popup()
        if not probe:
            return False, "could not reopen the popup to read the pointer after the click"
        match = re.search(OPEN_LINE, probe)
        click_x, click_y = int(match.group(4)), int(match.group(5))
        if not ensure_closed():
            return False, "could not close the pointer-check popup"
        if px <= click_x < px + width and py <= click_y < py + height:
            return False, f"the click dismissed the popup, but the pointer was inside at ({click_x}, {click_y})"
        print(f"   click at ({click_x}, {click_y}) closed the popup at ({px}, {py}) sized {width} x {height}")
        return True, "Escape and a click outside the popup both close it"

    left_open = ensure_closed()
    if left_open:
        print(f"note: a popup was already open ({left_open.split('] ')[1]}), closed it\n")

    for letter, gate in (("A", gate_a), ("B", gate_b), ("C", gate_c),
                         ("D", gate_d), ("E", gate_e), ("F", gate_f)):
        try:
            ok, detail = gate()
        except (RuntimeError, subprocess.TimeoutExpired, subprocess.CalledProcessError) as err:
            ok, detail = False, f"the gate could not run: {err}"
            print(f"   {err}")
        record(letter, ok, detail)

    print("Summary")
    print("=======")
    for letter, ok, detail in results:
        verdict = "MEASURED" if ok is None else ("PASS" if ok else "FAIL")
        print(f"  {letter}: {verdict} - {detail}")

    failed = [r for r in results if r[1] is False]
    if failed:
        print(f"\n{len(failed)} gate(s) failed.")
        return 1
    print("\nEvery gate with a measurable threshold passed. Gate E measured shell responsiveness.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
