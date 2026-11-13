# Private helpers for the supported shell grammar. Native forms pass argv data
# to a separately gated Rust command effect, never Python-generated shell source.
import sys as __rig_xonsh_sys
from pathlib import Path as __rig_xonsh_path


def __rig_xonsh_word(value):
    # Path conversion is lexical argument data, not a filesystem operation or
    # generic coercion of arbitrary objects through their __fspath__ methods.
    if isinstance(value, __rig_xonsh_path):
        value = str(value)
    if not isinstance(value, str):
        raise TypeError("shell arguments must be strings or Paths")
    if "\x00" in value:
        raise ValueError("shell arguments cannot contain NUL")
    return value


def __rig_xonsh_quote(value):
    return "'" + value.replace("'", "'\\''") + "'"


def __rig_xonsh_run(pipeline, capture):
    commands = []
    for command in pipeline:
        arguments = []
        for kind, value in command:
            if kind == "splice" and isinstance(value, list):
                for item in value:
                    arguments.append(__rig_xonsh_word(item))
            else:
                arguments.append(__rig_xonsh_word(value))
        if not arguments:
            raise ValueError("shell command has no arguments")
        commands.append(arguments)
    if __rig_xonsh_backend == "native":
        reply = __rig_xonsh_native(commands)
    else:
        quoted = []
        for command in commands:
            quoted.append(" ".join([__rig_xonsh_quote(word) for word in command]))
        script = "set -o pipefail; " + " | ".join(quoted)
        files = [{"path": name, "content": content} for name, content in __rig_xonsh_files.items()]
        reply = tool("virtual_bash", {"script": script, "files": files})
    if reply["status"] != "success":
        raise RuntimeError("shell command " + reply["status"] + ": " + str(reply["data"]))
    data = reply["data"]
    if __rig_xonsh_backend == "native":
        if data["status"] != {"exited": 0}:
            raise RuntimeError("native command failed: " + data["output"])
        if data["truncated"]:
            raise RuntimeError("native command output was truncated")
        stdout = data["output"]
        stderr = ""
    else:
        if data["exit_code"] != 0:
            raise RuntimeError("virtual command failed: " + data["stderr"])
        if data["stdout_truncated"] or data["stderr_truncated"]:
            raise RuntimeError("virtual command output was truncated")
        stdout = data["stdout"]
        stderr = data["stderr"]
    if stderr:
        print(stderr, end="", file=__rig_xonsh_sys.stderr)
    if capture:
        return stdout
    print(stdout, end="")
    return None
