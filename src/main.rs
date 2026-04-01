use std::{
    cmp::Ordering,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail};
use rustyline::{
    Context as RustylineContext, Editor, Helper,
    completion::{Completer, Pair},
    config::{CompletionType, Configurer},
    error::ReadlineError,
    highlight::Highlighter,
    hint::Hinter,
    history::DefaultHistory,
    validate::Validator,
};
use tokio::{runtime::Handle, task::block_in_place};
use zookeeper_client::{Acls, Client, CreateMode, Error as ZkError, Stat};

const COMMANDS: &[&str] = &[
    "connect", "auth", "ls", "cd", "pwd", "get", "stat", "exists", "create", "set", "setv",
    "delete", "delv", "help", "quit", "exit",
];

const DEFAULT_SERVERS: &str = "127.0.0.1:2181";

#[tokio::main]
async fn main() -> Result<()> {
    let mut repl = Repl::default();
    repl.run().await
}

struct Repl {
    session: Option<Session>,
    cwd: String,
    completion_state: Arc<Mutex<CompletionState>>,
}

struct Session {
    client: Client,
    auth_summary: String,
}

#[derive(Default)]
struct CompletionState {
    cwd: String,
    client: Option<Client>,
}

#[derive(Clone, Copy)]
enum PathCompletionMode {
    Full,
    ParentOnly,
}

struct ReplHelper {
    state: Arc<Mutex<CompletionState>>,
}

struct DeleteArgs<'a> {
    recursive: bool,
    expected_version: Option<i32>,
    path: &'a str,
}

struct GetArgs {
    mode: GetMode,
    path: String,
}

enum GetMode {
    Text,
    Hex,
    Version,
}

enum ReplAction {
    Continue,
    Exit,
}

impl Default for Repl {
    fn default() -> Self {
        Self {
            session: None,
            cwd: "/".to_string(),
            completion_state: Arc::new(Mutex::new(CompletionState {
                cwd: "/".to_string(),
                client: None,
            })),
        }
    }
}

impl Repl {
    async fn run(&mut self) -> Result<()> {
        print_banner();
        let mut editor: Editor<ReplHelper, DefaultHistory> =
            Editor::new().context("failed to initialize line editor")?;
        editor.set_completion_type(CompletionType::List);
        editor.set_helper(Some(ReplHelper {
            state: Arc::clone(&self.completion_state),
        }));

        loop {
            let input = match editor.readline(&self.prompt()) {
                Ok(line) => line,
                Err(ReadlineError::Interrupted) => {
                    println!("^C");
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    println!();
                    break;
                }
                Err(error) => return Err(error).context("failed to read command line"),
            };

            let input = input.trim();
            if input.is_empty() {
                continue;
            }

            editor
                .add_history_entry(input)
                .context("failed to record history entry")?;

            match self.execute(input).await {
                Ok(ReplAction::Continue) => {}
                Ok(ReplAction::Exit) => break,
                Err(error) => eprintln!("error: {error:#}"),
            }
        }

        Ok(())
    }

    fn prompt(&self) -> String {
        if self.session.is_some() {
            format!("zkctl:{} > ", self.cwd)
        } else {
            "zkctl> ".to_string()
        }
    }

    async fn execute(&mut self, input: &str) -> Result<ReplAction> {
        let (command, rest) = split_command(input);

        match command {
            "help" => {
                print_help();
                Ok(ReplAction::Continue)
            }
            "quit" | "exit" => Ok(ReplAction::Exit),
            "connect" => {
                self.command_connect(rest).await?;
                Ok(ReplAction::Continue)
            }
            "auth" => {
                self.command_auth(rest).await?;
                Ok(ReplAction::Continue)
            }
            "pwd" => {
                ensure_no_args(rest, "usage: pwd")?;
                println!("{}", self.cwd);
                Ok(ReplAction::Continue)
            }
            "cd" => {
                self.command_cd(rest).await?;
                Ok(ReplAction::Continue)
            }
            "ls" => {
                self.command_ls(rest).await?;
                Ok(ReplAction::Continue)
            }
            "get" => {
                self.command_get(rest).await?;
                Ok(ReplAction::Continue)
            }
            "setv" => {
                self.command_setv(rest).await?;
                Ok(ReplAction::Continue)
            }
            "stat" => {
                self.command_stat(rest).await?;
                Ok(ReplAction::Continue)
            }
            "exists" => {
                self.command_exists(rest).await?;
                Ok(ReplAction::Continue)
            }
            "create" => {
                self.command_create(rest).await?;
                Ok(ReplAction::Continue)
            }
            "set" => {
                self.command_set(rest).await?;
                Ok(ReplAction::Continue)
            }
            "delete" => {
                self.command_delete(rest).await?;
                Ok(ReplAction::Continue)
            }
            "delv" => {
                self.command_delv(rest).await?;
                Ok(ReplAction::Continue)
            }
            unknown => bail!("unknown command '{unknown}'. run 'help' for available commands"),
        }
    }

    async fn command_connect(&mut self, rest: &str) -> Result<()> {
        let servers = if rest.trim().is_empty() {
            DEFAULT_SERVERS
        } else {
            parse_single_arg(rest, "usage: connect [host:port[,host:port]]")?
        };
        let client = Client::connect(servers)
            .await
            .with_context(|| format!("failed to connect to {servers}"))?;

        self.session = Some(Session {
            client,
            auth_summary: "anonymous".to_string(),
        });
        self.cwd = "/".to_string();
        self.sync_completion_state();

        println!("connected to {servers} (anonymous)");
        Ok(())
    }

    async fn command_auth(&mut self, rest: &str) -> Result<()> {
        let session = self.require_session_mut()?;
        let (scheme, remainder) = take_token(rest).context("usage: auth digest <user:pass>")?;
        if scheme != "digest" {
            bail!("only 'auth digest <user:pass>' is supported");
        }

        let credential = remainder.trim();
        if credential.is_empty() {
            bail!("usage: auth digest <user:pass>");
        }

        session
            .client
            .auth("digest", credential.as_bytes())
            .await
            .context("digest authentication failed")?;

        let username = credential.split(':').next().unwrap_or("unknown");
        session.auth_summary = format!("digest:{username}");
        println!("authenticated as digest:{username}");

        Ok(())
    }

    async fn command_cd(&mut self, rest: &str) -> Result<()> {
        let raw_path = parse_single_arg(rest, "usage: cd <path>")?;
        let path = self.resolve_path(raw_path)?;
        let session = self.require_session()?;

        let stat = session
            .client
            .check_stat(&path)
            .await
            .with_context(|| format!("failed to check {path}"))?;

        if stat.is_none() {
            bail!("path not found: {path}");
        }

        self.cwd = path;
        self.sync_completion_state();
        Ok(())
    }

    async fn command_ls(&self, rest: &str) -> Result<()> {
        let path = self.resolve_optional_path(rest, "/")?;
        let session = self.require_session()?;

        let (mut children, _stat) = session
            .client
            .get_children(&path)
            .await
            .with_context(|| format!("failed to list children for {path}"))?;

        children.sort_by(|left, right| natural_cmp(left, right));

        if children.is_empty() {
            println!("(no children)");
        } else {
            for child in children {
                println!("{child}");
            }
        }

        Ok(())
    }

    async fn command_get(&self, rest: &str) -> Result<()> {
        let get_args = self.parse_get_args(rest)?;
        let session = self.require_session()?;

        let (data, _stat) = session
            .client
            .get_data(&get_args.path)
            .await
            .with_context(|| format!("failed to read node {}", get_args.path))?;

        match get_args.mode {
            GetMode::Hex => {
                println!("{}", format_hex(&data));
                Ok(())
            }
            GetMode::Version => {
                println!("{}", _stat.version);
                Ok(())
            }
            GetMode::Text => {
                match String::from_utf8(data) {
                    Ok(text) => {
                        if text.is_empty() {
                            println!("<empty>");
                        } else if text.ends_with('\n') {
                            print!("{text}");
                        } else {
                            println!("{text}");
                        }
                    }
                    Err(_) => println!(
                        "binary data; run 'get --hex {}' to inspect bytes",
                        get_args.path
                    ),
                }

                Ok(())
            }
        }
    }

    async fn command_setv(&self, rest: &str) -> Result<()> {
        let session = self.require_session()?;
        let (expected_version, raw_path, value) = parse_setv_args(rest)?;
        let path = self.resolve_path(raw_path)?;

        let stat = match session
            .client
            .set_data(&path, value.as_bytes(), Some(expected_version))
            .await
        {
            Ok(stat) => stat,
            Err(error) => {
                return Err(write_error_with_version(&session.client, &path, error, "update").await);
            }
        };

        println!("updated {path} to version {}", stat.version);
        Ok(())
    }

    async fn command_stat(&self, rest: &str) -> Result<()> {
        let path = self.resolve_optional_path(rest, "/")?;
        let session = self.require_session()?;

        let stat = session
            .client
            .check_stat(&path)
            .await
            .with_context(|| format!("failed to stat {path}"))?
            .with_context(|| format!("path not found: {path}"))?;

        print_stat(&path, stat);
        Ok(())
    }

    async fn command_exists(&self, rest: &str) -> Result<()> {
        let path = self.resolve_optional_path(rest, "/")?;
        let session = self.require_session()?;

        let exists = session
            .client
            .check_stat(&path)
            .await
            .with_context(|| format!("failed to check {path}"))?
            .is_some();

        println!("{}", if exists { "exists" } else { "not found" });
        Ok(())
    }

    async fn command_create(&self, rest: &str) -> Result<()> {
        let session = self.require_session()?;
        let (raw_path, value) = parse_path_and_value(rest, false)?;
        let path = self.resolve_path(raw_path)?;

        let options = CreateMode::Persistent.with_acls(Acls::anyone_all());
        let _ = session
            .client
            .create(&path, value.as_bytes(), &options)
            .await
            .with_context(|| format!("failed to create {path}"))?;

        println!("created {path}");
        Ok(())
    }

    async fn command_set(&self, rest: &str) -> Result<()> {
        let session = self.require_session()?;
        let (expected_version, raw_path, value) = parse_set_args(rest)?;
        let path = self.resolve_path(raw_path)?;

        let stat = match session
            .client
            .set_data(&path, value.as_bytes(), expected_version)
            .await
        {
            Ok(stat) => stat,
            Err(error) => {
                return Err(write_error_with_version(&session.client, &path, error, "update").await);
            }
        };

        println!("updated {path} to version {}", stat.version);
        Ok(())
    }

    async fn command_delete(&self, rest: &str) -> Result<()> {
        let session = self.require_session()?;
        let delete_args = parse_delete_args(rest)?;
        let path = self.resolve_path(delete_args.path)?;

        if delete_args.recursive {
            if path == "/" {
                bail!("refusing to recursively delete the root node '/'");
            }

            let deleted = delete_recursive(&session.client, &path).await?;
            println!("deleted {deleted} nodes under {path}");
            return Ok(());
        }

        match session
            .client
            .delete(&path, delete_args.expected_version)
            .await
        {
            Ok(()) => {}
            Err(error) => {
                return Err(write_error_with_version(&session.client, &path, error, "delete").await);
            }
        }

        println!("deleted {path}");
        Ok(())
    }

    async fn command_delv(&self, rest: &str) -> Result<()> {
        let session = self.require_session()?;
        let delete_args = parse_delv_args(rest)?;
        let path = self.resolve_path(delete_args.path)?;

        match session
            .client
            .delete(&path, delete_args.expected_version)
            .await
        {
            Ok(()) => {}
            Err(error) => {
                return Err(write_error_with_version(&session.client, &path, error, "delete").await);
            }
        }

        println!("deleted {path}");
        Ok(())
    }

    fn parse_get_args(&self, rest: &str) -> Result<GetArgs> {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            return Ok(GetArgs {
                mode: GetMode::Text,
                path: self.cwd.clone(),
            });
        }

        let (first, remainder) =
            take_token(trimmed).context("usage: get [--hex|--version] [path]")?;
        if first == "--hex" {
            if remainder.trim().is_empty() {
                return Ok(GetArgs {
                    mode: GetMode::Hex,
                    path: self.cwd.clone(),
                });
            }

            let raw_path = parse_single_arg(remainder, "usage: get [--hex|--version] [path]")?;
            return Ok(GetArgs {
                mode: GetMode::Hex,
                path: self.resolve_path(raw_path)?,
            });
        }

        if first == "--version" {
            if remainder.trim().is_empty() {
                return Ok(GetArgs {
                    mode: GetMode::Version,
                    path: self.cwd.clone(),
                });
            }

            let raw_path = parse_single_arg(remainder, "usage: get [--hex|--version] [path]")?;
            return Ok(GetArgs {
                mode: GetMode::Version,
                path: self.resolve_path(raw_path)?,
            });
        }

        ensure_no_args(remainder, "usage: get [--hex|--version] [path]")?;
        Ok(GetArgs {
            mode: GetMode::Text,
            path: self.resolve_path(first)?,
        })
    }

    fn resolve_optional_path(&self, rest: &str, _default: &str) -> Result<String> {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            return Ok(self.cwd.clone());
        }

        let raw_path = parse_single_arg(trimmed, "expected at most one path argument")?;
        self.resolve_path(raw_path)
    }

    fn resolve_path(&self, raw_path: &str) -> Result<String> {
        normalize_path(&self.cwd, raw_path)
    }

    fn require_session(&self) -> Result<&Session> {
        self.session
            .as_ref()
            .context("not connected. run 'connect <host:port[,host:port]>' first")
    }

    fn require_session_mut(&mut self) -> Result<&mut Session> {
        self.session
            .as_mut()
            .context("not connected. run 'connect <host:port[,host:port]>' first")
    }

    fn sync_completion_state(&self) {
        if let Ok(mut state) = self.completion_state.lock() {
            state.cwd = self.cwd.clone();
            state.client = self.session.as_ref().map(|session| session.client.clone());
        }
    }
}

impl Helper for ReplHelper {}

impl Hinter for ReplHelper {
    type Hint = String;
}

impl Highlighter for ReplHelper {}

impl Validator for ReplHelper {}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &RustylineContext<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let before_cursor = &line[..pos];
        let token_start = before_cursor
            .rfind(char::is_whitespace)
            .map(|index| index + 1)
            .unwrap_or(0);
        let current_token = &before_cursor[token_start..];
        let tokens_before = before_cursor[..token_start]
            .split_whitespace()
            .collect::<Vec<_>>();

        if tokens_before.is_empty() {
            return Ok((token_start, complete_command_names(current_token)));
        }

        let command = tokens_before[0];
        let arg_index = tokens_before.len();

        if let Some(option_names) = option_candidates(command, arg_index) {
            if current_token.starts_with('-') {
                return Ok((
                    token_start,
                    complete_fixed_values(current_token, option_names),
                ));
            }
        }

        let Some(mode) = path_completion_mode(command, arg_index, &tokens_before) else {
            return Ok((token_start, Vec::new()));
        };

        Ok((token_start, self.complete_path_token(current_token, mode)))
    }
}

impl ReplHelper {
    fn complete_path_token(&self, raw_token: &str, mode: PathCompletionMode) -> Vec<Pair> {
        let Some((cwd, client)) = self.snapshot() else {
            return Vec::new();
        };

        let replacements = match mode {
            PathCompletionMode::Full => existing_path_replacements(&cwd, &client, raw_token),
            PathCompletionMode::ParentOnly => parent_path_replacements(&cwd, &client, raw_token),
        };

        replacements
            .into_iter()
            .map(|replacement| Pair {
                display: replacement.clone(),
                replacement,
            })
            .collect()
    }

    fn snapshot(&self) -> Option<(String, Client)> {
        let state = self.state.lock().ok()?;
        Some((state.cwd.clone(), state.client.clone()?))
    }
}

fn print_banner() {
    println!("zkctl REPL");
    println!("run 'help' to see available commands");
}

fn print_help() {
    println!("Commands:");
    println!("  connect [host:port[,host:port]]   connect to ZooKeeper");
    println!("  auth digest <user:pass>           add digest auth to current session");
    println!("  ls [path]                         list child nodes");
    println!("  cd <path>                         change current node");
    println!("  pwd                               print current node");
    println!("  get [path]                        print node data as UTF-8");
    println!("  get --hex [path]                  print node data as hex");
    println!("  get --version [path]              print the current znode version");
    println!("  stat [path]                       print node stat metadata");
    println!("  exists [path]                     check whether a node exists");
    println!("  create <path> [value]             create a persistent node");
    println!("  set <path> <value>                update node data");
    println!("  set --version <N> <path> <value>  update node data with version check");
    println!("  setv <N> <path> <value>           alias for version-checked update");
    println!("  delete <path>                     delete a node");
    println!("  delete --version <N> <path>       delete a node with version check");
    println!("  delete --recursive <path>         delete a node and all descendants");
    println!("  delete -r <path>                  alias for recursive delete");
    println!("  delv <N> <path>                   alias for version-checked delete");
    println!("  help                              show this help text");
    println!("  quit | exit                       leave the REPL");
    println!();
    println!("Notes:");
    println!("  - Tab completes command names and ZooKeeper paths");
    println!("  - connect with no arguments uses 127.0.0.1:2181");
    println!("  - relative paths are resolved from the current prompt path");
    println!("  - values may contain spaces: set feature_flags/enabled true false");
    println!("  - surrounding single or double quotes are stripped: set /app/msg \"hello world\"");
    println!("  - set/delete accept --version N to avoid overwriting concurrent changes");
    println!("  - when a version check fails, zkctl prints the server's current version");
    println!("  - recursive delete prints progress, is fail-fast, and refuses to delete '/'");
}

fn complete_command_names(prefix: &str) -> Vec<Pair> {
    complete_fixed_values(prefix, COMMANDS)
}

fn complete_fixed_values(prefix: &str, values: &[&str]) -> Vec<Pair> {
    values
        .iter()
        .filter(|value| value.starts_with(prefix))
        .map(|value| Pair {
            display: (*value).to_string(),
            replacement: (*value).to_string(),
        })
        .collect()
}

fn option_candidates(command: &str, arg_index: usize) -> Option<&'static [&'static str]> {
    match (command, arg_index) {
        ("set", 1) => Some(&["--version"]),
        ("get", 1) => Some(&["--hex", "--version"]),
        ("delete", 1) => Some(&["--recursive", "-r", "--version"]),
        _ => None,
    }
}

fn path_completion_mode(
    command: &str,
    arg_index: usize,
    tokens_before: &[&str],
) -> Option<PathCompletionMode> {
    match command {
        "ls" | "cd" | "stat" | "exists" if arg_index == 1 => Some(PathCompletionMode::Full),
        "set" if arg_index == 1 => Some(PathCompletionMode::Full),
        "set" if arg_index == 3 && tokens_before.get(1) == Some(&"--version") => {
            Some(PathCompletionMode::Full)
        }
        "setv" if arg_index == 2 => Some(PathCompletionMode::Full),
        "create" if arg_index == 1 => Some(PathCompletionMode::ParentOnly),
        "get" if arg_index == 1 => Some(PathCompletionMode::Full),
        "get"
            if arg_index == 2
                && matches!(tokens_before.get(1), Some(&"--hex") | Some(&"--version")) =>
        {
            Some(PathCompletionMode::Full)
        }
        "delete" if arg_index == 1 => Some(PathCompletionMode::Full),
        "delete" if arg_index == 3 && tokens_before.get(1) == Some(&"--version") => {
            Some(PathCompletionMode::Full)
        }
        "delv" if arg_index == 2 => Some(PathCompletionMode::Full),
        "delete"
            if arg_index == 2
                && matches!(tokens_before.get(1), Some(&"--recursive") | Some(&"-r")) =>
        {
            Some(PathCompletionMode::Full)
        }
        _ => None,
    }
}

fn split_command(input: &str) -> (&str, &str) {
    let trimmed = input.trim();
    match trimmed.find(char::is_whitespace) {
        Some(index) => (&trimmed[..index], trimmed[index..].trim_start()),
        None => (trimmed, ""),
    }
}

fn take_token(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    match trimmed.find(char::is_whitespace) {
        Some(index) => Some((&trimmed[..index], trimmed[index..].trim_start())),
        None => Some((trimmed, "")),
    }
}

fn parse_single_arg<'a>(input: &'a str, usage: &str) -> Result<&'a str> {
    let (arg, rest) = take_token(input).context(usage.to_string())?;
    ensure_no_args(rest, usage)?;
    Ok(arg)
}

fn ensure_no_args(rest: &str, usage: &str) -> Result<()> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        bail!(usage.to_string())
    }
}

fn parse_path_and_value<'a>(input: &'a str, require_value: bool) -> Result<(&'a str, String)> {
    let (path, remainder) = take_token(input).context(if require_value {
        "usage: set <path> <value>"
    } else {
        "usage: create <path> [value]"
    })?;

    let trimmed = remainder.trim();
    if require_value && trimmed.is_empty() {
        bail!("usage: set <path> <value>");
    }

    Ok((path, decode_value(trimmed)))
}

fn parse_set_args(input: &str) -> Result<(Option<i32>, &str, String)> {
    let Some((first, remainder)) = take_token(input) else {
        bail!("usage: set [--version <N>] <path> <value>");
    };

    if first == "--version" {
        let (version_token, remainder) =
            take_token(remainder).context("usage: set --version <N> <path> <value>")?;
        let expected_version = parse_version_number(version_token, "set")?;
        let (path, value) = parse_path_and_value(remainder, true)?;
        return Ok((Some(expected_version), path, value));
    }

    let (path, value) = parse_path_and_value(input, true)?;
    Ok((None, path, value))
}

fn parse_setv_args(input: &str) -> Result<(i32, &str, String)> {
    let (version_token, remainder) = take_token(input).context("usage: setv <N> <path> <value>")?;
    let expected_version = parse_version_number(version_token, "setv")?;
    let (path, value) = parse_path_and_value(remainder, true)?;
    Ok((expected_version, path, value))
}

fn parse_delete_args(input: &str) -> Result<DeleteArgs<'_>> {
    let (first, remainder) =
        take_token(input).context("usage: delete [--version <N>|--recursive|-r] <path>")?;

    if matches!(first, "--recursive" | "-r") {
        let path = parse_single_arg(remainder, "usage: delete [--recursive|-r] <path>")?;
        return Ok(DeleteArgs {
            recursive: true,
            expected_version: None,
            path,
        });
    }

    if first == "--version" {
        let (version_token, remainder) =
            take_token(remainder).context("usage: delete --version <N> <path>")?;
        let expected_version = parse_version_number(version_token, "delete")?;
        let path = parse_single_arg(remainder, "usage: delete --version <N> <path>")?;
        return Ok(DeleteArgs {
            recursive: false,
            expected_version: Some(expected_version),
            path,
        });
    }

    ensure_no_args(remainder, "usage: delete <path>")?;
    Ok(DeleteArgs {
        recursive: false,
        expected_version: None,
        path: first,
    })
}

fn parse_delv_args(input: &str) -> Result<DeleteArgs<'_>> {
    let (version_token, remainder) = take_token(input).context("usage: delv <N> <path>")?;
    let expected_version = parse_version_number(version_token, "delv")?;
    let path = parse_single_arg(remainder, "usage: delv <N> <path>")?;
    Ok(DeleteArgs {
        recursive: false,
        expected_version: Some(expected_version),
        path,
    })
}

fn parse_version_number(raw: &str, command: &str) -> Result<i32> {
    raw.parse::<i32>()
        .with_context(|| format!("{command} version must be a signed 32-bit integer"))
}

fn decode_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }

    trimmed.to_string()
}

fn normalize_path(cwd: &str, raw_path: &str) -> Result<String> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        bail!("path cannot be empty");
    }

    let mut parts = if raw_path.starts_with('/') {
        Vec::new()
    } else {
        cwd.split('/')
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };

    for part in raw_path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            segment => parts.push(segment.to_string()),
        }
    }

    if parts.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

fn existing_path_replacements(cwd: &str, client: &Client, raw_token: &str) -> Vec<String> {
    let Ok((parent_path, replacement_prefix, name_prefix)) = full_path_lookup(cwd, raw_token)
    else {
        return Vec::new();
    };

    list_matching_children(client, &parent_path, &name_prefix)
        .into_iter()
        .map(|child| format!("{replacement_prefix}{child}"))
        .collect()
}

fn parent_path_replacements(cwd: &str, client: &Client, raw_token: &str) -> Vec<String> {
    if raw_token.is_empty() {
        return list_matching_children(client, cwd, "")
            .into_iter()
            .map(|child| format!("{child}/"))
            .collect();
    }

    if raw_token.ends_with('/') {
        let Ok((parent_path, replacement_prefix, _)) = full_path_lookup(cwd, raw_token) else {
            return Vec::new();
        };

        return list_matching_children(client, &parent_path, "")
            .into_iter()
            .map(|child| format!("{replacement_prefix}{child}/"))
            .collect();
    }

    let Some(separator_index) = raw_token.rfind('/') else {
        return Vec::new();
    };

    let leaf_suffix = &raw_token[separator_index + 1..];
    let parent_token = if separator_index == 0 && raw_token.starts_with('/') {
        "/"
    } else {
        &raw_token[..separator_index]
    };

    let parent_replacements = if parent_token == "/" {
        vec!["/".to_string()]
    } else {
        existing_path_replacements(cwd, client, parent_token)
    };

    parent_replacements
        .into_iter()
        .map(|parent| {
            if parent == "/" {
                format!("/{leaf_suffix}")
            } else {
                format!("{parent}/{leaf_suffix}")
            }
        })
        .collect()
}

fn full_path_lookup(cwd: &str, raw_token: &str) -> Result<(String, String, String)> {
    if raw_token.is_empty() {
        return Ok((cwd.to_string(), String::new(), String::new()));
    }

    if raw_token.ends_with('/') {
        let parent_input = if raw_token == "/" {
            "/"
        } else {
            raw_token.trim_end_matches('/')
        };
        let parent_path = normalize_path(cwd, parent_input)?;
        return Ok((parent_path, raw_token.to_string(), String::new()));
    }

    if let Some(separator_index) = raw_token.rfind('/') {
        let parent_input = if separator_index == 0 && raw_token.starts_with('/') {
            "/"
        } else {
            &raw_token[..separator_index]
        };
        let parent_path = normalize_path(cwd, parent_input)?;
        let replacement_prefix = raw_token[..separator_index + 1].to_string();
        let name_prefix = raw_token[separator_index + 1..].to_string();
        return Ok((parent_path, replacement_prefix, name_prefix));
    }

    Ok((cwd.to_string(), String::new(), raw_token.to_string()))
}

fn list_matching_children(client: &Client, parent_path: &str, name_prefix: &str) -> Vec<String> {
    let client = client.clone();
    let parent_path = parent_path.to_string();
    let mut children = block_in_place(|| {
        Handle::current().block_on(async move {
            client
                .get_children(&parent_path)
                .await
                .map(|(children, _)| children)
                .unwrap_or_default()
        })
    });

    children.sort_by(|left, right| natural_cmp(left, right));
    children
        .into_iter()
        .filter(|child| child.starts_with(name_prefix))
        .collect()
}

async fn write_error_with_version(
    client: &Client,
    path: &str,
    error: ZkError,
    action: &str,
) -> anyhow::Error {
    let base = format!("failed to {action} {path}: {error}");
    if error != ZkError::BadVersion {
        return anyhow!(base);
    }

    match client.check_stat(path).await {
        Ok(Some(stat)) => anyhow!(format!("{base} (server version: {})", stat.version)),
        Ok(None) => anyhow!(format!("{base} (node no longer exists)")),
        Err(stat_error) => anyhow!(format!(
            "{base} (also failed to fetch current version: {stat_error})"
        )),
    }
}

async fn delete_recursive(client: &Client, path: &str) -> Result<usize> {
    let delete_order = collect_delete_order(client, path).await?;
    let total = delete_order.len();

    println!("deleting {total} nodes under {path}");

    for (index, current_path) in delete_order.iter().enumerate() {
        client
            .delete(current_path, None)
            .await
            .with_context(|| format!("failed to delete {current_path}"))?;
        println!("deleted [{}/{}] {}", index + 1, total, current_path);
    }

    Ok(total)
}

async fn collect_delete_order(client: &Client, path: &str) -> Result<Vec<String>> {
    let mut stack = vec![(path.to_string(), false)];
    let mut delete_order = Vec::new();

    while let Some((current_path, visited_children)) = stack.pop() {
        if visited_children {
            delete_order.push(current_path);
            continue;
        }

        stack.push((current_path.clone(), true));

        let (children, _stat) = client
            .get_children(&current_path)
            .await
            .with_context(|| format!("failed to list children for {current_path}"))?;

        for child in children.into_iter().rev() {
            stack.push((join_path(&current_path, &child), false));
        }
    }

    Ok(delete_order)
}

fn join_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn print_stat(path: &str, stat: Stat) {
    println!("path: {path}");
    println!("version: {}", stat.version);
    println!("children: {}", stat.num_children);
    println!("bytes: {}", stat.data_length);
    println!(
        "ephemeral: {}",
        if stat.ephemeral_owner == 0 {
            "no"
        } else {
            "yes"
        }
    );
    println!("czxid: {}", stat.czxid);
    println!("mzxid: {}", stat.mzxid);
    println!("pzxid: {}", stat.pzxid);
    println!("ctime: {}", stat.ctime);
    println!("mtime: {}", stat.mtime);
    println!("cversion: {}", stat.cversion);
    println!("aversion: {}", stat.aversion);
}

fn format_hex(data: &[u8]) -> String {
    if data.is_empty() {
        return "<empty>".to_string();
    }

    data.chunks(16)
        .enumerate()
        .map(|(index, chunk)| {
            let offset = format!("{:08x}", index * 16);
            let hex = chunk
                .iter()
                .map(|byte| format!("{:02x}", byte))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = chunk
                .iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>();

            format!("{offset}  {hex:<47}  {ascii}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left_num), Ok(right_num)) => left_num.cmp(&right_num),
        _ => left.cmp(right),
    }
}
