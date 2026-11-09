# Private helpers for the supported shell grammar. The frontend emits data IR,
# never executable shell source from Python interpolation.
import sys as __rig_xonsh_sys


def __rig_xonsh_quote(value):
    if not isinstance(value, str):
        raise TypeError("shell arguments must be strings")
    if "\x00" in value:
        raise ValueError("shell arguments cannot contain NUL")
    return "'" + value.replace("'", "'\\''") + "'"


def __rig_xonsh_run(pipeline, capture):
    commands = []
    for command in pipeline:
        arguments = []
        for kind, value in command:
            if kind == "splice" and isinstance(value, list):
                for item in value:
                    arguments.append(__rig_xonsh_quote(item))
            else:
                arguments.append(__rig_xonsh_quote(value))
        if not arguments:
            raise ValueError("shell command has no arguments")
        commands.append(" ".join(arguments))
    script = "set -o pipefail; " + " | ".join(commands)
    if __rig_xonsh_backend == "native":
        reply = tool("native_shell", {"script": script})
    else:
        files = [{"path": name, "content": content} for name, content in __rig_xonsh_files.items()]
        reply = tool("virtual_bash", {"script": script, "files": files})
    if reply["status"] != "success":
        raise RuntimeError("shell tool " + reply["status"] + ": " + str(reply["data"]))
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
