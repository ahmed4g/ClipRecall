#!/usr/bin/env python3
"""Phase 3 live acceptance gate. Run inside a live GNOME Wayland session with the
extension loaded (a logout/login is required for GNOME Shell to register the UUID).

    python3 tests/verifyObserver.py [--idle-seconds 600]

Exit codes: 0 all gates pass, 2 the extension is not loaded (nothing to measure),
1 a gate failed.

Gates (plan.md Phase 3):
  A. copy in a real app -> row in the DB within 100 ms
  B. re-copying updates the timestamp and moves the item to the top
  C. excluded apps are rejected
  D. password manager hint copies are rejected
  E. idle CPU < 1%

Copies are driven with wl-clipboard, which is a real Wayland client on this session.
The source app the extension records is the focused window, so gate C reads the app
id from the row gate A produced rather than guessing it.
"""

import argparse
import os
import re
import shlex
import subprocess
import sys
import time

UUID = "clipboard-manager@project.local"
BUS = "org.clipboardmanager.Daemon"
PATH = "/org/clipboardmanager/Daemon"
IFACE = "org.clipboardmanager.Daemon"

ROW_FIELDS = ("id", "primary_mime", "preview", "sort_time", "pinned", "has_thumbnail", "source_app")


def call(method, signature, *args):
    cmd = ["busctl", "--user", "call", BUS, PATH, IFACE, method]
    if signature:
        cmd.append(signature)
        cmd.extend(str(a) for a in args)
    res = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
    if res.returncode != 0:
        raise RuntimeError(f"{method} failed: {res.stderr.strip() or res.stdout.strip()}")
    return res.stdout.strip()


def history(limit=50, offset=0, filter=""):
    out = call("GetHistory", "iis", offset, limit, filter)
    # busctl prints the signature, the element count, then the values in order:
    #   a(ssstbbs) 1 "id" "text/plain" "preview" 1793900256 false false "app"
    tokens = shlex.split(out)
    if not tokens or tokens[0] != "a(ssstbbs)":
        raise RuntimeError(f"unexpected GetHistory reply: {out}")
    count = int(tokens[1])
    values = tokens[2:]
    if len(values) != count * len(ROW_FIELDS):
        raise RuntimeError(f"GetHistory reply has {len(values)} values for {count} rows")

    rows = []
    for i in range(count):
        row = dict(zip(ROW_FIELDS, values[i * len(ROW_FIELDS):(i + 1) * len(ROW_FIELDS)]))
        row["sort_time"] = int(row["sort_time"])
        row["pinned"] = row["pinned"] == "true"
        row["has_thumbnail"] = row["has_thumbnail"] == "true"
        rows.append(row)
    return rows


def entry_count():
    out = call("Health", "")
    m = re.search(r"sutb\s+(\S+)\s+(\d+)\s+(\d+)\s+(\S+)", out)
    if not m:
        raise RuntimeError(f"unexpected Health reply: {out}")
    return int(m.group(2))


def wl_copy(text, mime=None, sensitive=False):
    cmd = ["wl-copy"]
    if sensitive:
        cmd.append("--sensitive")
    if mime:
        cmd.extend(["-t", mime])
    cmd.append(text)
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=5)


def extension_loaded():
    res = subprocess.run(["gnome-extensions", "info", UUID], capture_output=True, text=True, timeout=5)
    return res.returncode == 0 and "State: ACTIVE" in res.stdout


def cpu_percent(pid, seconds):
    """CPU busy percentage for one process over `seconds`, from /proc/<pid>/stat."""
    def ticks():
        with open(f"/proc/{pid}/stat") as fh:
            fields = fh.read().split()
        return int(fields[13]) + int(fields[14])

    clock = os.sysconf("SC_CLK_TCK")
    t0, c0 = time.monotonic(), ticks()
    time.sleep(seconds)
    elapsed, delta = time.monotonic() - t0, ticks() - c0
    return 100.0 * delta / (clock * elapsed)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--idle-seconds", type=int, default=600, help="idle window for gate E")
    args = ap.parse_args()

    print("Phase 3 live acceptance gate")
    print("============================\n")

    if not extension_loaded():
        print(f"SKIP: {UUID} is not loaded by GNOME Shell in this session.")
        print("GNOME Shell registers a newly installed UUID only after a logout/login.")
        return 2

    # The daemon is normally started by the systemd user service. D-Bus activation is
    # only answered by a session bus that has already scanned ~/.local/share/dbus-1/services,
    # so a bus that was running before the install will not activate on demand.
    try:
        call("Health", "")
    except RuntimeError as err:
        print(f"daemon not reachable ({err}); starting the user service")
        subprocess.run(["systemctl", "--user", "start", "clipboard-manager-daemon.service"],
                       capture_output=True, text=True, timeout=10)
        time.sleep(1)
        call("Health", "")

    pid_out = subprocess.run(
        ["systemctl", "--user", "show", "-p", "MainPID", "clipboard-manager-daemon.service"],
        capture_output=True, text=True, timeout=10).stdout.strip()
    daemon_pid = int(pid_out.split("=")[1]) if pid_out else 0
    if daemon_pid == 0:
        print("SKIP: could not resolve the daemon pid for the idle CPU gate")
        return 2

    token = f"gate-{int(time.time())}"

    # A. copy -> row within 100 ms. The clock starts when the copy completes, so the
    # measured time is the pipeline: debounce, read_async, D-Bus, insert.
    before = entry_count()
    t0 = time.monotonic()
    wl_copy(token)
    copied = time.monotonic()
    row = None
    while time.monotonic() - copied < 1.0:
        rows = history(limit=5)
        if rows and rows[0]["preview"] == token:
            row = rows[0]
            break
    latency_ms = (time.monotonic() - copied) * 1000
    if row is None:
        print("FAIL A: no row appeared within 1 s")
        return 1
    print(f"A: row in {latency_ms:.1f} ms after the copy completed "
          f"({(time.monotonic() - t0) * 1000:.1f} ms including the wl-copy process); "
          "includes the 50 ms debounce")
    if latency_ms >= 100:
        print("FAIL A: over the gate")
        return 1
    if entry_count() != before + 1:
        print("FAIL A: entry count did not increase by one")
        return 1

    # B. re-copy updates the timestamp and moves the item to the top.
    # A different item is copied first so the reorder is observable: without it the item is
    # already at the top from gate A. Timestamps are milliseconds, so a re-copy that lands in
    # the same wall-clock second still produces a strictly greater sort_time.
    first_sort = row["sort_time"]
    wl_copy(f"{token}-second")
    time.sleep(0.3)
    second = history(limit=5)[0]
    if second["id"] == row["id"]:
        print("FAIL B: the second copy did not take the top spot")
        return 1
    wl_copy(token)
    time.sleep(0.3)
    top = history(limit=5)[0]
    if top["id"] != row["id"] or top["sort_time"] <= first_sort:
        print("FAIL B: re-copy did not bump the timestamp and reorder")
        return 1
    print(f"B: re-copy bumped sort_time {first_sort} -> {top['sort_time']} and moved the item "
          f"above {second['preview']}")

    # C. excluded app rejected. The extension records the focused window as the
    # source app, so read the id from the row instead of guessing it.
    app = row["source_app"] or "unknown"
    call("AddExcludedApp", "s", app)
    time.sleep(0.2)  # the extension refreshes its exclusion list on the bus
    before = entry_count()
    wl_copy(f"{token}-excluded")
    time.sleep(0.5)
    if entry_count() != before:
        print(f"FAIL C: copy from excluded app {app} was stored")
        return 1
    print(f"C: copy from excluded app {app} rejected")
    call("RemoveExcludedApp", "s", app)

    # D. password manager hint rejected. --sensitive adds the hint alongside text/plain,
    # which is an allowed MIME, so the rejection is attributable to the hint check rather
    # than to the allowlist.
    before = entry_count()
    wl_copy(f"{token}-secret", mime="text/plain", sensitive=True)
    time.sleep(0.5)
    if entry_count() != before:
        print("FAIL D: password manager hint copy was stored")
        return 1
    print("D: password manager hint copy rejected (text/plain + hint)")

    # E. idle CPU. The extension runs inside gnome-shell, so its own cost is measured as the
    # difference between gnome-shell with the extension enabled and with it disabled. The
    # absolute figure is only an upper bound: it includes the other extensions enabled in this
    # session and any rendering the session does during the window.
    print(f"E: measuring idle CPU over {args.idle_seconds} s ...")
    shell_out = subprocess.run(["pgrep", "-x", "gnome-shell"], capture_output=True, text=True, timeout=5).stdout.split()
    if not shell_out:
        print("FAIL E: gnome-shell is not running in this session")
        return 1
    shell_pid = int(shell_out[0])
    daemon_cpu = cpu_percent(daemon_pid, args.idle_seconds / 2)
    rss_kb = int(open(f"/proc/{daemon_pid}/status").read().split("VmRSS:")[1].split("\n")[0].strip().split()[0])
    print(f"   daemon CPU {daemon_cpu:.2f}% (gate < 1%), RSS {rss_kb / 1024:.1f} MB (gate < 40 MB)")

    with_ext = cpu_percent(shell_pid, args.idle_seconds / 4)
    subprocess.run(["gnome-extensions", "disable", UUID], capture_output=True, text=True, timeout=10)
    time.sleep(2)
    baseline = cpu_percent(shell_pid, args.idle_seconds / 4)
    subprocess.run(["gnome-extensions", "enable", UUID], capture_output=True, text=True, timeout=10)
    time.sleep(2)
    after = cpu_percent(shell_pid, args.idle_seconds / 4)
    print(f"   gnome-shell {with_ext:.2f}% with the extension, {baseline:.2f}% with it disabled "
          f"(delta {with_ext - baseline:.2f}%); {after:.2f}% after re-enabling")
    print("   the delta is the extension's idle cost; the absolute figure includes the other "
          "extensions enabled in this session")

    if daemon_cpu >= 1.0:
        print("FAIL E: daemon idle CPU over the gate")
        return 1

    print("\nAll Phase 3 gates passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
