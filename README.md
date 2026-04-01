# zkctl

`zkctl` is a ZooKeeper REPL for browsing and editing znodes from a shell-like prompt.

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

- Relative paths are resolved from the current prompt path.
- `set <path> <value>` treats everything after `<path>` as the value.
- Surrounding single or double quotes are stripped from values.
- `delete` is non-recursive.
