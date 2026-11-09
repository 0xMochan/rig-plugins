# rig-bashkit

`BashkitPlugin` adds `virtual_bash`: a short shell calculation over text passed
with the request. Each call creates a fresh in-memory filesystem, puts the input
files under `/workspace`, and discards everything after returning stdout,
stderr, exit status and truncation flags.

```json
{
  "script": "cat data/items.txt | sort | uniq",
  "files": [{"path": "data/items.txt", "content": "pear\napple\npear\n"}]
}
```

This tool does not mount the project, inherit host environment variables, run
native programs, or access the network. Use the native shell tool for `cargo`,
`git`, `uv`, `npm`, and actual project files. An unsupported command fails; there
is no automatic retry outside the virtual interpreter. Generated virtual files
are not copied into the project; print the desired result.

Limits are fixed by the host: 64 KiB script, 64 input files totaling 1 MiB,
4 MiB virtual filesystem data, 1,000 commands, five seconds, and 64 KiB captured
raw bytes per output stream. The capture cap applies to the returned output;
intermediate command output, pipelines and redirected file contents can exceed
it within Bashkit's separate work, memory and filesystem budgets. Those budgets
do not constitute an OS process-memory limit. Bashkit's other parser, loop and
interpreter-memory defaults remain in effect. Outputs are presented as UTF-8
text with replacement for invalid bytes, so decoding can expand the byte count
of the returned strings.

The dependency disables Bashkit's default features and enables no optional
host capabilities. Bashkit supplies in-process isolation and resource checks;
this is not a native-process OS sandbox. The separate native-shell plugin owns
that boundary and approval policy.

The public `execute(&Request)` helper exercises the same boundary as the tool
and requires a Tokio runtime. Inside Rig, the plugin uses the harness's existing
`TokioRuntime`; dropping a tool call aborts its task. No interpreter, mutex, or
files are shared between calls.

The small test set checks useful transformations and discarded state, absence
of host/native capabilities, and the host's input/work/output bounds. It does
not duplicate Bashkit's shell compatibility suite.
