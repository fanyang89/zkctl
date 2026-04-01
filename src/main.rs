use std::{
    cmp::Ordering,
    io::{self, Write},
};

use anyhow::{Context, Result, bail};
use zookeeper_client::{Acls, Client, CreateMode, Stat};

#[tokio::main]
async fn main() -> Result<()> {
    let mut repl = Repl::default();
    repl.run().await
}

struct Repl {
    session: Option<Session>,
    cwd: String,
}

struct Session {
    client: Client,
    auth_summary: String,
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
        }
    }
}

impl Repl {
    async fn run(&mut self) -> Result<()> {
        print_banner();

        loop {
            print!("{}", self.prompt());
            io::stdout().flush().context("failed to flush stdout")?;

            let mut input = String::new();
            let bytes = io::stdin()
                .read_line(&mut input)
                .context("failed to read stdin")?;

            if bytes == 0 {
                println!();
                break;
            }

            let input = input.trim();
            if input.is_empty() {
                continue;
            }

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
            unknown => bail!("unknown command '{unknown}'. run 'help' for available commands"),
        }
    }

    async fn command_connect(&mut self, rest: &str) -> Result<()> {
        let servers = parse_single_arg(rest, "usage: connect <host:port[,host:port]>")?;
        let client = Client::connect(servers)
            .await
            .with_context(|| format!("failed to connect to {servers}"))?;

        self.session = Some(Session {
            client,
            auth_summary: "anonymous".to_string(),
        });
        self.cwd = "/".to_string();

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
        let (hex, path) = self.parse_get_args(rest)?;
        let session = self.require_session()?;

        let (data, _stat) = session
            .client
            .get_data(&path)
            .await
            .with_context(|| format!("failed to read node {path}"))?;

        if hex {
            println!("{}", format_hex(&data));
            return Ok(());
        }

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
            Err(_) => println!("binary data; run 'get --hex {path}' to inspect bytes"),
        }

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
        let (raw_path, value) = parse_path_and_value(rest, true)?;
        let path = self.resolve_path(raw_path)?;

        let stat = session
            .client
            .set_data(&path, value.as_bytes(), None)
            .await
            .with_context(|| format!("failed to update {path}"))?;

        println!("updated {path} to version {}", stat.version);
        Ok(())
    }

    async fn command_delete(&self, rest: &str) -> Result<()> {
        let session = self.require_session()?;
        let raw_path = parse_single_arg(rest, "usage: delete <path>")?;
        let path = self.resolve_path(raw_path)?;

        session
            .client
            .delete(&path, None)
            .await
            .with_context(|| format!("failed to delete {path}"))?;

        println!("deleted {path}");
        Ok(())
    }

    fn parse_get_args(&self, rest: &str) -> Result<(bool, String)> {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            return Ok((false, self.cwd.clone()));
        }

        let (first, remainder) = take_token(trimmed).context("usage: get [--hex] [path]")?;
        if first == "--hex" {
            if remainder.trim().is_empty() {
                return Ok((true, self.cwd.clone()));
            }

            let raw_path = parse_single_arg(remainder, "usage: get [--hex] [path]")?;
            return Ok((true, self.resolve_path(raw_path)?));
        }

        ensure_no_args(remainder, "usage: get [--hex] [path]")?;
        Ok((false, self.resolve_path(first)?))
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
}

fn print_banner() {
    println!("zkctl REPL");
    println!("run 'help' to see available commands");
}

fn print_help() {
    println!("Commands:");
    println!("  connect <host:port[,host:port]>   connect to ZooKeeper");
    println!("  auth digest <user:pass>           add digest auth to current session");
    println!("  ls [path]                         list child nodes");
    println!("  cd <path>                         change current node");
    println!("  pwd                               print current node");
    println!("  get [path]                        print node data as UTF-8");
    println!("  get --hex [path]                  print node data as hex");
    println!("  stat [path]                       print node stat metadata");
    println!("  exists [path]                     check whether a node exists");
    println!("  create <path> [value]             create a persistent node");
    println!("  set <path> <value>                update node data");
    println!("  delete <path>                     delete a node (non-recursive)");
    println!("  help                              show this help text");
    println!("  quit | exit                       leave the REPL");
    println!();
    println!("Notes:");
    println!("  - relative paths are resolved from the current prompt path");
    println!("  - values may contain spaces: set feature_flags/enabled true false");
    println!("  - surrounding single or double quotes are stripped: set /app/msg \"hello world\"");
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
