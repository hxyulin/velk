"""An LSP server whose child holds a Cargo-style build-directory lock."""
import json
import pathlib
import subprocess
import sys
import time

root = pathlib.Path(sys.argv[1])
mode = sys.argv[2]
child = subprocess.Popen(
    [
        sys.executable,
        "-c",
        "import fcntl,pathlib,sys,time; "
        "root=pathlib.Path(sys.argv[1]); "
        "lock=open(root/'.cargo-lock','a'); "
        "fcntl.flock(lock,fcntl.LOCK_EX); "
        "(root/'locked').write_text('ready'); "
        "time.sleep(60)",
        str(root),
    ],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
while not (root / "locked").exists():
    if child.poll() is not None:
        raise RuntimeError("lock-holder child exited before startup")
    time.sleep(0.01)


def send(message):
    body = json.dumps({"jsonrpc": "2.0", **message}).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


send({"method": "fixture/ready", "params": {"child": child.pid}})
while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line == b"\r\n":
            break
        name, value = line.decode().strip().split(":", 1)
        headers[name.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    if message.get("method") == "shutdown" and mode != "unresponsive":
        send({"id": message["id"], "result": None})
    elif message.get("method") == "exit":
        sys.exit(0)
