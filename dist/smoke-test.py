#!/usr/bin/env python3
"""End-to-end smoke test: boots a headless Orchard in an isolated environment, opens a real
Wayland client, drives every major code path over IPC and fails if the compositor dies,
logs a panic, or stops answering.

It never touches the running session: own XDG_RUNTIME_DIR (own Wayland + IPC sockets),
own XDG_CONFIG_HOME (default settings), no autostart.

usage: dist/smoke-test.py [path/to/target/dir]   (default: target/debug)
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "debug"))
COMPOSITOR = os.path.join(BIN, "orchard")
CLIENT = os.path.join(BIN, "orchard-settings")

failures = []


def step(name):
    print(f"  · {name}", flush=True)


def fail(msg):
    failures.append(msg)
    print(f"  ✗ {msg}", flush=True)


class Ipc:
    def __init__(self, path):
        self.path = path

    def request(self, req, timeout=3.0):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout)
            s.connect(self.path)
            s.sendall((json.dumps(req) + "\n").encode())
            data = b""
            while not data.endswith(b"\n"):
                chunk = s.recv(65536)
                if not chunk:
                    break
                data += chunk
            return json.loads(data)

    def raw(self, payload: bytes):
        """Send raw bytes, return whatever comes back (or None)."""
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(2.0)
            s.connect(self.path)
            try:
                s.sendall(payload)
                return s.recv(65536)
            except (BrokenPipeError, ConnectionResetError, socket.timeout):
                return None


def wait_for(pred, timeout, what):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = pred()
            if v:
                return v
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    fail(f"timed out waiting for {what}")
    return None


def main():
    for b in (COMPOSITOR, CLIENT):
        if not os.access(b, os.X_OK):
            print(f"missing {b} (build first)", file=sys.stderr)
            return 2

    tmp = tempfile.mkdtemp(prefix="orchard-smoke-")
    runtime = os.path.join(tmp, "run")
    config = os.path.join(tmp, "config")
    os.makedirs(runtime, mode=0o700)
    os.makedirs(os.path.join(config, "orchard"))
    log_path = os.path.join(tmp, "orchard.log")

    env = {k: v for k, v in os.environ.items()
           if k not in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "ORCHARD_SOCKET", "DISPLAY")}
    env.update(
        XDG_RUNTIME_DIR=runtime,
        XDG_CONFIG_HOME=config,
        XDG_STATE_HOME=os.path.join(tmp, "state"),
        ORCHARD_NO_AUTOSTART="1",
        ORCHARD_HEADLESS_OUTPUTS="2",
        RUST_LOG=env.get("RUST_LOG", "info"),
        PATH=BIN + os.pathsep + env.get("PATH", ""),
    )

    print(f"smoke test: {COMPOSITOR}\n  scratch: {tmp}")
    log = open(log_path, "w")
    proc = subprocess.Popen([COMPOSITOR, "--headless"], env=env, stdout=log, stderr=subprocess.STDOUT)

    def alive(after):
        if proc.poll() is not None:
            fail(f"compositor exited (code {proc.returncode}) after: {after}")
            return False
        return True

    try:
        step("startup")
        sock = wait_for(lambda: next((os.path.join(runtime, f) for f in os.listdir(runtime)
                                      if f.startswith("orchard-wayland") and f.endswith(".sock")), None),
                        20, "IPC socket")
        if not sock:
            return 1
        ipc = Ipc(sock)
        outputs = ipc.request({"request": "outputs"})
        if outputs.get("event") == "error" or not outputs.get("outputs"):
            fail(f"outputs: {outputs}")

        step("IPC rejects garbage without dying")
        ipc.raw(b"this is not json\n")
        ipc.raw(b'{"request":"no_such_request"}\n')
        ipc.raw(b"\xff\xfe\x00\n")
        ipc.raw(b"x" * (2 << 20))  # no newline, over the request cap
        alive("garbage IPC")

        step("open a window")
        ipc.request({"request": "spawn", "command": CLIENT})
        if not wait_for(lambda: ipc.request({"request": "windows"}).get("windows"), 20, "a window to map"):
            return 1

        actions = ["zoom", "zoom", "mission-control", "mission-control", "app-expose", "app-expose",
                   "space-right", "space-left", "move-space-right", "space-right", "space-left",
                   "move-space-left", "fullscreen", "fullscreen", "minimize", "unminimize",
                   "switcher-next", "switcher-commit", "launcher", "launcher", "cycle", "cycle-app"]
        step(f"{len(actions)} window-management actions")
        for a in actions:
            r = ipc.request({"request": "action", "name": a})
            if r.get("event") == "error":
                fail(f"action {a}: {r}")
            time.sleep(0.25)  # let animations run a few frames
            if not alive(f"action {a}"):
                return 1

        step("screenshot")
        shot = os.path.join(tmp, "shot.png")
        with open(os.path.join(runtime, "orchard-ctl"), "a") as ctl:
            ctl.write(f"shot {shot}\n")
        if wait_for(lambda: os.path.exists(shot) and os.path.getsize(shot) > 1000, 10, "screenshot"):
            print(f"    {shot}")

        step("reload a broken settings file")
        settings = os.path.join(config, "orchard", "settings.json")
        for text in ["{ broken", '{"appearance":{"mode":"dark","accent":"ultraviolet"},"dock":5}']:
            with open(settings, "w") as f:
                f.write(text)
            ipc.request({"request": "reload_settings"})
            time.sleep(0.3)
            if not alive("reload_settings with a broken file"):
                return 1
        if not os.path.exists(settings + ".bad"):
            fail("broken settings were not backed up to settings.json.bad")

        step("close the window")
        for w in ipc.request({"request": "windows"}).get("windows", []):
            ipc.request({"request": "close", "id": w["id"]})
        time.sleep(1.0)
        alive("closing windows")

        step("quit")
        ipc.request({"request": "action", "name": "quit"})
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            fail("compositor did not quit within 10 s")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        log.close()

    text = open(log_path, errors="replace").read()
    if "PANIC" in text or "panicked at" in text:
        fail("panic in the log")
    if "orchard-shell exited" in text:
        fail("orchard-shell crashed and was restarted")
    if failures:
        print(f"\nFAILED ({len(failures)}) — log: {log_path}")
        print("".join(text.splitlines(True)[-40:]))
        return 1
    print("smoke test passed")
    shutil.rmtree(tmp, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
