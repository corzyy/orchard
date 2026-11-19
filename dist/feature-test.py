#!/usr/bin/env python3
"""Headless feature test harness. Run inside `dbus-run-session`.
usage: ftest.py <script.py>  -- the script gets `ipc`, `ctl(line)`, `shot(name)`, `sh(cmd)`, `sleep`
"""
import json, os, socket, subprocess, sys, tempfile, time

ROOT = os.path.expanduser("~/projects/orchard")
BIN = os.path.join(ROOT, "target", os.environ.get("PROFILE", "debug"))
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "shots")
os.makedirs(OUT, exist_ok=True)


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
            return json.loads(data) if data else None

    def send(self, req):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.connect(self.path)
            s.sendall((json.dumps(req) + "\n").encode())
            time.sleep(0.05)


tmp = tempfile.mkdtemp(prefix="orchard-ft-")
runtime = os.path.join(tmp, "run")
config = os.path.join(tmp, "config")
os.makedirs(runtime, mode=0o700)
os.makedirs(os.path.join(config, "orchard"))
env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "ORCHARD_SOCKET", "DISPLAY")}
env.update(XDG_RUNTIME_DIR=runtime, XDG_CONFIG_HOME=config, XDG_STATE_HOME=os.path.join(tmp, "state"),
           ORCHARD_NO_AUTOSTART="1", RUST_LOG=env.get("RUST_LOG", "info"),
           PIPEWIRE_RUNTIME_DIR="/run/user/%d" % os.getuid(),
           PATH=BIN + os.pathsep + env.get("PATH", ""))
env.update(json.loads(os.environ.get("FT_ENV", "{}")))
log_path = os.path.join(tmp, "orchard.log")
log = open(log_path, "w")
proc = subprocess.Popen([os.path.join(BIN, "orchard"), "--headless"], env=env, stdout=log, stderr=subprocess.STDOUT)
try:
    end = time.time() + 20
    sock = None
    while time.time() < end and not sock:
        sock = next((os.path.join(runtime, f) for f in os.listdir(runtime) if f.startswith("orchard-wayland") and f.endswith(".sock")), None)
        time.sleep(0.1)
    ipc = Ipc(sock)
    wl = os.path.basename(sock)[len("orchard-"):-len(".sock")]
    cenv = dict(env, WAYLAND_DISPLAY=wl, ORCHARD_SOCKET=sock)
    time.sleep(2.0)

    def ctl(line):
        with open(os.path.join(runtime, "orchard-ctl"), "a") as f:
            f.write(line + "\n")
        time.sleep(0.15)

    def shot(name):
        p = os.path.join(OUT, name + ".png")
        if os.path.exists(p):
            os.remove(p)
        ctl(f"shot {p}")
        end = time.time() + 10
        while time.time() < end and not (os.path.exists(p) and os.path.getsize(p) > 1000):
            time.sleep(0.1)
        time.sleep(0.2)
        print("shot", p)
        return p

    def sh(cmd):
        return subprocess.run(cmd, shell=True, env=cenv, capture_output=True, text=True)

    exec(open(sys.argv[1]).read())
    ipc.send({"request": "action", "name": "quit"})
    proc.wait(10)
finally:
    if proc.poll() is None:
        proc.kill()
    log.close()
    text = open(log_path, errors="replace").read()
    print("---- log (filtered) ----")
    for l in text.splitlines():
        if any(k in l for k in ("WARN", "ERROR", "PANIC", "panicked", "notification", "session", "orchard_shell")):
            print(l[:300])
    print("log:", log_path)
