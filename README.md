# zkctl

`zkctl` is a ZooKeeper REPL for browsing and editing znodes from a shell-like prompt.

The REPL uses readline-style input, so you get in-session command history and line editing out of the box.

## Run

```bash
cargo run
```

After startup you will see a prompt like:

```text
zkctl>
```

Connect first:

```text
zkctl> connect 127.0.0.1:2181
connected to 127.0.0.1:2181 (anonymous)
zkctl:/ >
```

## Commands

- `connect <host:port[,host:port]>`
- `auth digest <user:pass>`
- `ls [path]`
- `cd <path>`
- `pwd`
- `get [path]`
- `get --hex [path]`
- `stat [path]`
- `exists [path]`
- `create <path> [value]`
- `set <path> <value>`
- `delete <path>`
- `delete --recursive <path>`
- `help`
- `quit` / `exit`

## Examples

```text
zkctl> connect 127.0.0.1:2181
zkctl:/ > ls
app
config

zkctl:/ > cd /app
zkctl:/app > get feature_flag
enabled

zkctl:/app > set feature_flag disabled
updated /app/feature_flag to version 2

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
- Relative paths are resolved from the current prompt path.
- `set <path> <value>` treats everything after `<path>` as the value.
- Surrounding single or double quotes are stripped from values.
- `delete` is non-recursive unless you pass `--recursive`.
- `delete --recursive` prints progress, is fail-fast, and refuses to delete `/`.
