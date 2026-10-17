"""Deterministic LSP peer for integration tests; no third-party Python packages."""
import json
import os
import sys
import time

log_path, mode = sys.argv[1:]
completion_requests = 0
if mode == "runtime-stop":
    print("fixture server started", file=sys.stderr, flush=True)

def log(message):
    with open(log_path, "a", encoding="utf-8") as file:
        file.write(json.dumps(message, ensure_ascii=False) + "\n")

def send(message):
    body = json.dumps(dict(jsonrpc="2.0", **message), ensure_ascii=False).encode("utf-8")
    # Deliberately fragment the header/body writes.
    frame = f"Content-Length: {len(body)}\r\n\r\n".encode() + body
    for part in [frame[:7], frame[7:23], frame[23:]]:
        sys.stdout.buffer.write(part)
        sys.stdout.buffer.flush()

def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        name, value = line.decode().split(":", 1)
        if name.lower() == "content-length":
            length = int(value)
    message = json.loads(sys.stdin.buffer.read(length))
    log(message)
    return message

def offset(text, position):
    lines = text.splitlines(keepends=True)
    line = position["line"]
    before = "".join(lines[:line])
    content = lines[line] if line < len(lines) else ""
    column = position["character"]
    # Test server deliberately selects UTF-16 despite client's UTF-8 preference.
    return len(before) + len(content.encode("utf-16-le")[:column * 2].decode("utf-16-le"))

log({"pid": os.getpid()})
uri, text, version, held = None, "", 0, None
documents = {}
workspace_published = False
held_pull = None
while (message := read()) is not None:
    method, params = message.get("method"), (message.get("params") or {})
    target = params.get("textDocument", {}).get("uri")
    if method == "textDocument/didClose":
        documents.pop(target, None)
        uri = None
        continue
    if target in documents:
        uri = target
        text, version = documents[target]
    if method == "initialize":
        if mode == "stall-init":
            while True:
                time.sleep(60)
        send({"id": message["id"], "result": {"capabilities": {
            "positionEncoding": "utf-16", "hoverProvider": True,
            **({"diagnosticProvider": {"identifier": "fixture", "interFileDependencies": True, "workspaceDiagnostics": False}} if mode.startswith("pull") else {}),
            "definitionProvider": True, "typeDefinitionProvider": True, "implementationProvider": True, "referencesProvider": True,
            "experimental": {"parentModule": True},
            "documentFormattingProvider": True, "renameProvider": True,
            "codeActionProvider": {"resolveProvider": True}, "completionProvider": {"resolveProvider": True, "triggerCharacters": [".", ":"]},
            "textDocumentSync": {"openClose": True, "change": 2, "save": {"includeText": True}}
        }}})
        send({"id": "config", "method": "workspace/configuration", "params": {"items": [{"section": "rust-analyzer"}]}})
        send({"id": "edit", "method": "workspace/applyEdit", "params": {"edit": {"changes": {}}}})
    elif method == "textDocument/didOpen":
        document = params["textDocument"]
        uri, text, version = document["uri"], document["text"], document["version"]
        if mode != "pull-stale":
            send({"method": "textDocument/publishDiagnostics", "params": {"uri": uri, "version": version, "diagnostics": [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}, "severity": 2, "message": "current"}]}})
        if mode == "workspace" and not workspace_published:
            workspace_published = True
            unopened = uri.rsplit("/", 1)[0] + "/unopened.rs"
            send({"method": "textDocument/publishDiagnostics", "params": {"uri": unopened, "diagnostics": [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 9}}, "severity": 1, "message": "unopened workspace error"}]}})
    elif method == "textDocument/didSave" and mode == "workspace":
        unopened = uri.rsplit("/", 1)[0] + "/unopened.rs"
        send({"method": "textDocument/publishDiagnostics", "params": {"uri": unopened, "diagnostics": []}})
    elif method == "textDocument/didChange":
        version = params["textDocument"]["version"]
        for change in params["contentChanges"]:
            if "range" in change:
                a, b = offset(text, change["range"]["start"]), offset(text, change["range"]["end"])
                text = text[:a] + change["text"] + text[b:]
            else:
                text = change["text"]
        log({"reconstructed": text, "version": version})
        if mode != "pull-stale" and not (mode.startswith("pull") and "fixed" in text):
            for diagnostic_version in [version - 1, version]:
                pushed = [] if mode.startswith("pull") and "fixed" in text else [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}, "severity": 2, "message": "current" if diagnostic_version == version else "stale"}]
                send({"method": "textDocument/publishDiagnostics", "params": {"uri": uri, "version": diagnostic_version, "diagnostics": pushed}})
    elif method == "textDocument/diagnostic":
        current_result = f"r{version}"
        items = [] if "fixed" in text else [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}, "severity": 1, "message": "pulled", "source": "fixture"}]
        if mode == "pull-stale" and held_pull is None:
            held_pull = message["id"]
            continue
        if mode == "pull-stale" and held_pull is not None:
            send({"id": held_pull, "result": {"kind": "full", "resultId": "stale", "items": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "severity": 1, "message": "STALE"}]}})
        if params.get("previousResultId") == current_result:
            send({"id": message["id"], "result": {"kind": "unchanged", "resultId": current_result}})
        else:
            send({"id": message["id"], "result": {"kind": "full", "resultId": current_result, "items": items}})
        if not items:
            send({"method": "textDocument/publishDiagnostics", "params": {"uri": uri, "version": version, "diagnostics": []}})
    elif method == "textDocument/didSave" and mode.startswith("pull"):
        send({"id": f"refresh-{version}", "method": "workspace/diagnostic/refresh", "params": None})
    elif method == "textDocument/formatting":
        send({"id": message["id"], "result": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "// formatted\n"}]})
    elif method == "textDocument/rename":
        target = {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}
        other = uri.rsplit("/", 1)[0] + "/other.rs"
        send({"id": message["id"], "result": {"documentChanges": [
            {"textDocument": {"uri": uri, "version": version}, "edits": [{"range": target, "newText": params["newName"]}]},
            {"textDocument": {"uri": other, "version": None}, "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}}, "newText": params["newName"]}]}
        ]}})
    elif method == "textDocument/codeAction":
        send({"id": message["id"], "result": [
            {"title": "Fix current symbol", "kind": "quickfix", "edit": {"changes": {uri: [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}, "newText": "fixed"}]}}},
            {"title": "Resolve a fix", "kind": "quickfix", "data": {"fix": "symbol"}},
            {"title": "Unsupported task", "command": "server.task"}
        ]})
    elif method == "codeAction/resolve":
        action = dict(params)
        action["edit"] = {"changes": {uri: [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}, "newText": "resolved"}]}}
        send({"id": message["id"], "result": action})
    elif method == "textDocument/completion":
        completion_requests += 1
        if mode == "incomplete-completion":
            items = [] if completion_requests == 2 else [{"label": "print"}]
            result = items if completion_requests >= 4 else {"isIncomplete": completion_requests < 3, "items": items}
            send({"id": message["id"], "result": result})
            continue
        send({"id": message["id"], "result": {"isIncomplete": False, "items": [
            {"label": "complete_call", "filterText": "call", "sortText": "001", "detail": "with an import", "textEdit": {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}, "newText": "complete_call()"}, "data": {"imports": True}},
            {"label": "snippet", "insertTextFormat": 2, "insertText": "call(${1:value})"}
        ]}})
    elif method == "completionItem/resolve":
        item = dict(params)
        item["documentation"] = {"kind": "markdown", "value": "**Complete a call**.\n\nUse `complete_call()` to insert a call with its import."}
        item["additionalTextEdits"] = [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "use support::complete_call;\n"}]
        send({"id": message["id"], "result": item})
    elif method in ["textDocument/definition", "textDocument/typeDefinition", "textDocument/implementation", "textDocument/references", "experimental/parentModule"]:
        if mode == "hold-command":
            continue
        target = {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 7}}
        if method == "textDocument/typeDefinition":
            send({"id": message["id"], "result": [{"targetUri": uri, "targetRange": target, "targetSelectionRange": target}]})
        elif method == "textDocument/definition":
            send({"id": message["id"], "result": {"uri": uri, "range": target}})
        else:
            send({"id": message["id"], "result": [{"uri": uri, "range": target}]})
    elif method == "rust-analyzer/expandMacro":
        send({"id": message["id"], "result": {"name": "example!", "expansion": "fn expanded() {}"}})
    elif method == "textDocument/hover":
        if mode == "timeout":
            continue
        if held is None:
            held = message["id"]
        else:
            send({"id": held, "result": {"contents": {"kind": "plaintext", "value": "STALE"}}})
            send({"id": message["id"], "result": {"contents": {"kind": "plaintext", "value": "CURRENT"}}})
    elif method == "shutdown":
        send({"id": message["id"], "result": None})
    elif method == "exit":
        if mode == "runtime-stop":
            print("fixture server stopped", file=sys.stderr, flush=True)
        break

    if uri is not None:
        documents[uri] = (text, version)
