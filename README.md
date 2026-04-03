# zkctl

`zkctl` is a ZooKeeper REPL for browsing and editing znodes from a shell-like prompt.

The REPL uses readline-style input, so you get in-session command history and line editing out of the box.

## Run

```bash
cargo run
```

Run commands directly without entering the REPL:

```bash
cargo run -- -c connect -c "ls /"
```

After startup you will see a prompt like:

```text
zkctl>
```

Connect first:

```text
zkctl> connect
connected to 127.0.0.1:2181 (anonymous)
zkctl:/ >
```

## Commands

- `connect [host:port[,host:port]]`
- `auth digest <user:pass>`
- `ls [path]`
- `cd <path>`
- `pwd`
- `get [path]`
- `get --hex [path]`
- `get --version [path]`
- `stat [path]`
- `exists [path]`
- `create <path> [value]`
- `set <path> <value>`
- `set --version <N> <path> <value>`
- `setv <N> <path> <value>`
- `delete <path>`
- `delete --version <N> <path>`
- `delete --recursive <path>`
- `delete -r <path>`
- `delv <N> <path>`
- `clear`
- `help`
- `quit` / `exit`

## Examples

```text
zkctl> connect
zkctl:/ > ls
app
config

zkctl:/ > cd /app
zkctl:/app > get feature_flag
enabled

zkctl:/app > set feature_flag disabled
updated /app/feature_flag to version 2

zkctl:/app > set --version 2 feature_flag enabled
updated /app/feature_flag to version 3

zkctl:/app > get --version feature_flag
3

zkctl:/app > create greeting "hello world"
created /app/greeting

zkctl:/app > stat greeting
path: /app/greeting
version: 0
children: 0
bytes: 11
```

## Notes

- Up and down arrow keys navigate command history for the current session.
- Left and right arrow keys, Home, End, Backspace, and Delete work during line editing.
- `Ctrl-R` performs reverse history search.
- `Ctrl-C` cancels the current input line.
- `Ctrl-D` exits the REPL.
- `Tab` completes command names and ZooKeeper paths.
- `connect` with no arguments uses `127.0.0.1:2181`.
- Relative paths are resolved from the current prompt path.
- `set <path> <value>` treats everything after `<path>` as the value.
- Surrounding single or double quotes are stripped from values.
- `set` and `delete` accept `--version <N>` to avoid overwriting concurrent changes.
- `setv` and `delv` are short aliases for version-checked write operations.
- When a version check fails, zkctl prints the server's current version.
- `delete` is non-recursive unless you pass `--recursive`.
- `delete -r` is an alias for `delete --recursive`.
- `delete --recursive` prints progress, is fail-fast, and refuses to delete `/`.
- `clear` deletes all user znodes under `/`, preserves `/zookeeper`, and asks for an explicit confirmation token before continuing.
- Use repeated `-c` or `--command` flags to run zkctl commands non-interactively, for example `zkctl -c connect -c "ls /"`.
- In direct execution mode, `clear` still requires confirmation: `zkctl -c connect -c clear -c CLEAR`.
