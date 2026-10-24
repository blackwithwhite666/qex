//! This module selects the part of a log file that a command shows.
//!
//! `qex logs` and `qex status` share these options, so the two commands always
//! behave in the same way.
//!
//! The reader is frequently an agent with a limited context. A job can write
//! hundreds of megabytes, so every path here has a limit, and every path says
//! how many lines it did not show. A reader must never believe that a part of
//! the output is the whole output.

use clap::Args;

/// The maximum number of lines that a command shows without an option.
pub const DEFAULT_LINES: usize = 500;

/// The maximum number of matches of `--grep` that a command shows.
///
/// A wide pattern can match every line. Without this limit, a search would
/// give the whole file to a reader who asked for a small part of it.
pub const DEFAULT_MATCHES: usize = 100;

/// The number of lines that `--follow` shows before it waits for more.
///
/// A job can already have written a large file when the command starts. Without
/// this limit, `--follow` would write that whole file first.
pub const FOLLOW_LEAD_LINES: usize = 20;

/// The number of lines that `qex status` shows for a job that failed.
///
/// This value is small on purpose. The command shows these lines with no option
/// at all, and the usual reader has a limited context.
pub const STATUS_LINES: usize = 20;

#[derive(Debug, Args, Clone, Default)]
pub struct LogSelect {
    /// Show the standard output only.
    #[arg(long, conflicts_with = "stderr", help_heading = "Log selection")]
    pub stdout: bool,

    /// Show the standard error only.
    #[arg(long, help_heading = "Log selection")]
    pub stderr: bool,

    /// Show the last N lines.
    #[arg(long, value_name = "N", help_heading = "Log selection")]
    pub tail: Option<usize>,

    /// Show the first N lines. Use this option for a fault at the start.
    #[arg(long, value_name = "N", help_heading = "Log selection")]
    pub head: Option<usize>,

    /// Show the lines from N to M. Example: --lines 4200:4230.
    #[arg(long, value_name = "N:M", help_heading = "Log selection")]
    pub lines: Option<String>,

    /// Show the lines that match this regular expression.
    #[arg(long, value_name = "REGEX", help_heading = "Log selection")]
    pub grep: Option<String>,

    /// Read the --grep value as plain text, and not as a regular expression.
    #[arg(long, help_heading = "Log selection")]
    pub fixed: bool,

    /// Show N lines before and after each match of --grep.
    #[arg(long, short = 'C', value_name = "N", help_heading = "Log selection")]
    pub context: Option<usize>,

    /// Write the line number before each line.
    #[arg(long, short = 'n', help_heading = "Log selection")]
    pub number: bool,

    /// The maximum number of matches of --grep to show. The default is 100.
    ///
    /// qex reports the number of matches that it did not show. This option has
    /// no meaning with --follow, which reads a stream and has no total.
    #[arg(long, value_name = "N", help_heading = "Log selection")]
    pub max_matches: Option<usize>,

    /// Show every line. Without this option, qex shows the last 500 lines.
    #[arg(long, help_heading = "Log selection")]
    pub all: bool,
}

/// The result of a selection.
#[derive(Debug)]
pub struct Selected {
    pub text: String,
    /// The number of lines that the selection did not show.
    pub hidden: usize,
    /// True when a limit cut the result.
    pub truncated: bool,
    /// The number of lines that matched `--grep`, before any limit.
    ///
    /// This number is useful by itself. A pattern with 3000 matches is too
    /// wide, and the reader learns that from this number.
    pub matches: Option<usize>,
    /// The number of matches that the output holds.
    pub matches_shown: usize,
}

/// Writes a count of lines with the correct verb: `1 line matches`.
fn lines_match(found: usize) -> String {
    if found == 1 {
        "1 line matches".to_string()
    } else {
        format!("{found} lines match")
    }
}

impl Selected {
    /// Gives one line that says what the output does not hold.
    ///
    /// A reader must never believe that a part of the output is the whole
    /// output, so every command writes this line when something is missing.
    pub fn notice(&self) -> Option<String> {
        match (self.matches, self.truncated) {
            // The singular and the plural are separate texts: `1 line matches`
            // and `2 lines match`. ASD-STE100 does not accept `line(s)`.
            (Some(found), _) if found > self.matches_shown => Some(format!(
                "... {}, and qex shows the first {}. \
                 Use `--max-matches N`, `--all`, or a narrower pattern.",
                lines_match(found),
                self.matches_shown
            )),
            (Some(found), _) => Some(format!("... {}.", lines_match(found))),
            (None, true) if self.hidden == 1 => Some(
                "... 1 earlier line is not shown. Use `--all`, `--head N` or `--tail N`."
                    .to_string(),
            ),
            (None, true) => Some(format!(
                "... {} earlier lines are not shown. Use `--all`, `--head N` or \
                 `--tail N`.",
                self.hidden
            )),
            (None, false) => None,
        }
    }
}

impl LogSelect {
    /// Tests the options against each other.
    ///
    /// `--follow` reads a stream, so an option that counts a total has no
    /// meaning with it.
    pub fn check_with_follow(&self) -> Result<(), String> {
        if self.max_matches.is_some() {
            return Err(
                "--max-matches has no meaning with --follow. A stream has no total, so qex \
                 cannot say how many matches come after a limit. Use --grep alone to filter \
                 the stream."
                    .to_string(),
            );
        }
        // `--tail N` DOES have a meaning with `--follow`: give the last N lines
        // that the job already wrote, then continue with the new lines. That is
        // the behaviour of `tail -f -n N`.
        for (name, given) in [
            ("--head", self.head.is_some()),
            ("--lines", self.lines.is_some()),
        ] {
            if given {
                return Err(format!(
                    "{name} has no meaning with --follow, because the output continues. \
                     Use --grep to filter the stream."
                ));
            }
        }
        Ok(())
    }

    /// Tests if the user gave any option that selects lines.
    pub fn is_explicit(&self) -> bool {
        self.tail.is_some()
            || self.head.is_some()
            || self.lines.is_some()
            || self.grep.is_some()
            || self.all
    }

    /// Chooses the part of the text to show.
    ///
    /// `default_limit` is the number of last lines to show when the user gives
    /// no option.
    pub fn apply(&self, text: &str, default_limit: usize) -> Result<Selected, String> {
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();

        // A search comes first. The other options then act on the matches.
        let (chosen, match_count, matches_shown) = match &self.grep {
            Some(pattern) => {
                let (lines_to_keep, found, shown) = self.search(&lines, pattern)?;
                (lines_to_keep, Some(found), shown)
            }
            None => ((0..total).collect(), None, 0),
        };

        // Choose a range of the chosen lines.
        //
        // Count the lines that each option leaves out. A reader must never
        // believe that a part of the output is the whole output.
        let available = chosen.len();
        let (picked, hidden): (Vec<usize>, usize) = if let Some(range) = &self.lines {
            let (from, to) = parse_range(range)?;
            // A person counts from 1.
            let kept: Vec<usize> = chosen
                .into_iter()
                .filter(|i| *i >= from.saturating_sub(1) && *i < to)
                .collect();
            let hidden = available.saturating_sub(kept.len());
            (kept, hidden)
        } else if let Some(n) = self.head {
            let kept: Vec<usize> = chosen.into_iter().take(n).collect();
            let hidden = available.saturating_sub(kept.len());
            (kept, hidden)
        } else if let Some(n) = self.tail {
            let start = available.saturating_sub(n);
            (chosen[start..].to_vec(), start)
        } else if self.all {
            (chosen, 0)
        } else {
            // No option. Show the last lines, and never the whole file.
            let start = available.saturating_sub(default_limit);
            (chosen[start..].to_vec(), start)
        };

        let mut out = String::new();
        let mut previous: Option<usize> = None;
        for i in picked.iter().copied() {
            // Show a mark where the selection leaves a gap, so a reader does not
            // read two lines as if they were together.
            if let Some(p) = previous {
                if i > p + 1 {
                    out.push_str("--\n");
                }
            }
            if self.number {
                out.push_str(&format!("{:>6}  ", i + 1));
            }
            out.push_str(lines[i]);
            out.push('\n');
            previous = Some(i);
        }

        Ok(Selected {
            text: out,
            hidden,
            truncated: hidden > 0,
            matches: match_count,
            matches_shown,
        })
    }

    /// Gives the numbers of the lines to show, with the context lines.
    ///
    /// The result also gives the number of lines that matched before the limit,
    /// and the number of matches in the output. A pattern that matches 3000
    /// lines is too wide, and the reader must learn that from the output.
    fn search(&self, lines: &[&str], pattern: &str) -> Result<(Vec<usize>, usize, usize), String> {
        let matcher = Matcher::new(pattern, self.fixed)?;
        let context = self.context.unwrap_or(0);

        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| matcher.matches(line))
            .map(|(i, _)| i)
            .collect();
        let found = hits.len();

        // Keep the FIRST matches. In a log, the first failure is usually the
        // cause, and the failures after it are the result of that cause.
        let limit = if self.all {
            found
        } else {
            self.max_matches.unwrap_or(DEFAULT_MATCHES)
        };
        let kept: Vec<usize> = hits.into_iter().take(limit).collect();
        let shown = kept.len();

        let mut keep = vec![false; lines.len()];
        for i in kept {
            let from = i.saturating_sub(context);
            let to = (i + context).min(lines.len().saturating_sub(1));
            for item in keep.iter_mut().take(to + 1).skip(from) {
                *item = true;
            }
        }

        Ok((
            keep.iter()
                .enumerate()
                .filter(|(_, k)| **k)
                .map(|(i, _)| i)
                .collect(),
            found,
            shown,
        ))
    }
}

/// Reads a range such as `4200:4230`.
fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let (a, b) = s.split_once(':').ok_or_else(|| {
        format!("incorrect range `{s}`. Use the form N:M, for example --lines 100:200.")
    })?;

    let from: usize = a
        .trim()
        .parse()
        .map_err(|_| format!("incorrect first line `{a}` in `{s}`"))?;
    let to: usize = b
        .trim()
        .parse()
        .map_err(|_| format!("incorrect last line `{b}` in `{s}`"))?;

    if from == 0 || to == 0 {
        return Err("a line number starts at 1".to_string());
    }
    if from > to {
        return Err(format!(
            "the first line {from} is after the last line {to} in `{s}`"
        ));
    }
    Ok((from, to))
}

/// Tests one line against a pattern.
///
/// qex has no regular expression library, so this type reads a small language:
/// `.`, `*`, `+`, `?`, `[abc]`, `^`, `$` and `|`. A pattern that uses more than
/// this gives a clear error, and `--fixed` accepts any text.
struct Matcher {
    alternatives: Vec<Vec<Token>>,
    anchored_start: bool,
    anchored_end: bool,
    fixed: Option<String>,
}

#[derive(Debug, Clone)]
enum Token {
    Char(char),
    Any,
    Class { chars: Vec<char>, negated: bool },
    Star(Box<Token>),
    Plus(Box<Token>),
    Optional(Box<Token>),
}

impl Matcher {
    fn new(pattern: &str, fixed: bool) -> Result<Self, String> {
        if fixed {
            return Ok(Self {
                alternatives: Vec::new(),
                anchored_start: false,
                anchored_end: false,
                fixed: Some(pattern.to_string()),
            });
        }

        let mut alternatives = Vec::new();
        let mut anchored_start = false;
        let mut anchored_end = false;

        for part in pattern.split('|') {
            let mut body = part;
            if let Some(rest) = body.strip_prefix('^') {
                anchored_start = true;
                body = rest;
            }
            if let Some(rest) = body.strip_suffix('$') {
                anchored_end = true;
                body = rest;
            }
            alternatives.push(parse_tokens(body)?);
        }

        Ok(Self {
            alternatives,
            anchored_start,
            anchored_end,
            fixed: None,
        })
    }

    fn matches(&self, line: &str) -> bool {
        if let Some(text) = &self.fixed {
            return line.contains(text.as_str());
        }

        let chars: Vec<char> = line.chars().collect();
        for tokens in &self.alternatives {
            let starts: Vec<usize> = if self.anchored_start {
                vec![0]
            } else {
                (0..=chars.len()).collect()
            };
            for start in starts {
                if let Some(end) = match_here(tokens, &chars, start) {
                    if !self.anchored_end || end == chars.len() {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn parse_tokens(pattern: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let token = match chars[i] {
            '.' => Token::Any,
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    return Err("the pattern ends with a backslash".to_string());
                }
                Token::Char(chars[i])
            }
            '[' => {
                let close = chars[i..].iter().position(|c| *c == ']').ok_or_else(|| {
                    format!("the group that starts at `[` has no `]` in `{pattern}`")
                })? + i;
                let mut set: Vec<char> = chars[i + 1..close].to_vec();
                let negated = set.first() == Some(&'^');
                if negated {
                    set.remove(0);
                }
                // Read a range such as a-z.
                let mut expanded = Vec::new();
                let mut k = 0;
                while k < set.len() {
                    if k + 2 < set.len() && set[k + 1] == '-' {
                        for c in set[k]..=set[k + 2] {
                            expanded.push(c);
                        }
                        k += 3;
                    } else {
                        expanded.push(set[k]);
                        k += 1;
                    }
                }
                i = close;
                Token::Class {
                    chars: expanded,
                    negated,
                }
            }
            '*' | '+' | '?' => {
                let previous = tokens.pop().ok_or_else(|| {
                    format!("`{}` has nothing before it in `{pattern}`", chars[i])
                })?;
                let wrapped = match chars[i] {
                    '*' => Token::Star(Box::new(previous)),
                    '+' => Token::Plus(Box::new(previous)),
                    _ => Token::Optional(Box::new(previous)),
                };
                tokens.push(wrapped);
                i += 1;
                continue;
            }
            '(' | ')' => {
                return Err(format!(
                    "this search does not read a group `(` or `)` in `{pattern}`. \
                     Use `--fixed` for plain text, or use `|` for alternatives."
                ))
            }
            c => Token::Char(c),
        };
        tokens.push(token);
        i += 1;
    }
    Ok(tokens)
}

/// Tests the tokens against the text from one position.
fn match_here(tokens: &[Token], chars: &[char], position: usize) -> Option<usize> {
    let Some((first, rest)) = tokens.split_first() else {
        return Some(position);
    };

    match first {
        Token::Star(inner) => {
            // Try the longest match first, then give characters back.
            let mut ends = vec![position];
            let mut p = position;
            while let Some(next) = match_one(inner, chars, p) {
                ends.push(next);
                p = next;
            }
            for end in ends.into_iter().rev() {
                if let Some(done) = match_here(rest, chars, end) {
                    return Some(done);
                }
            }
            None
        }
        Token::Plus(inner) => {
            let first_end = match_one(inner, chars, position)?;
            let mut ends = vec![first_end];
            let mut p = first_end;
            while let Some(next) = match_one(inner, chars, p) {
                ends.push(next);
                p = next;
            }
            for end in ends.into_iter().rev() {
                if let Some(done) = match_here(rest, chars, end) {
                    return Some(done);
                }
            }
            None
        }
        Token::Optional(inner) => {
            if let Some(next) = match_one(inner, chars, position) {
                if let Some(done) = match_here(rest, chars, next) {
                    return Some(done);
                }
            }
            match_here(rest, chars, position)
        }
        simple => {
            let next = match_one(simple, chars, position)?;
            match_here(rest, chars, next)
        }
    }
}

fn match_one(token: &Token, chars: &[char], position: usize) -> Option<usize> {
    if position >= chars.len() {
        return None;
    }
    let c = chars[position];
    let ok = match token {
        Token::Char(want) => c == *want,
        Token::Any => true,
        Token::Class {
            chars: set,
            negated,
        } => set.contains(&c) != *negated,
        // A repeat inside a repeat is not necessary here.
        _ => return None,
    };
    ok.then_some(position + 1)
}

// The cursor is an observation of retained files, not an authorization token.
// Keep its encoding private and versioned. No paths come from this data.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogCursor {
    v: u8,
    j: uuid::Uuid,
    s: Vec<LogPosition>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogPosition {
    n: String,
    // Device, inode, observed length and fingerprint of that entire prefix.
    // Include undisplayed bytes: rewriting those must invalidate the cursor too.
    m: Option<(u64, u64, u64, u64)>,
    o: u64,
}

impl LogCursor {
    fn encode(&self) -> String {
        let mut out = String::from("qlog1-");
        for b in serde_json::to_vec(self).expect("a log cursor serializes") {
            use std::fmt::Write;
            write!(out, "{b:02x}").unwrap();
        }
        out
    }

    fn decode(raw: &str) -> Option<Self> {
        let hex = raw.strip_prefix("qlog1-")?;
        if hex.len() > 4096 || hex.len() % 2 != 0 {
            return None;
        }
        let bytes: Option<Vec<u8>> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |c: u8| (c as char).to_digit(16).map(|n| n as u8);
                Some(digit(pair[0])? * 16 + digit(pair[1])?)
            })
            .collect();
        let cursor: Self = serde_json::from_slice(&bytes?).ok()?;
        (cursor.v == 1
            && cursor.s.len() <= 2
            && cursor
                .s
                .iter()
                .all(|p| p.m.map_or(p.o == 0, |(_, _, len, _)| p.o <= len)))
        .then_some(cursor)
    }
}

fn log_fingerprint(seed: u64, bytes: &[u8]) -> u64 {
    // FNV-1a, as used for other local fingerprints in qex. This detects
    // ordinary file changes; it is not a defense against hostile collisions.
    bytes.iter().fold(seed, |hash, b| {
        (hash ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

/// Scan a fixed retained prefix with constant scratch space, keeping only the
/// page candidate. Fingerprint the old observed prefix separately so rewriting
/// even an undisplayed byte invalidates the continuation.
fn scan_log(
    file: &mut std::fs::File,
    len: u64,
    previous_len: u64,
    offset: u64,
    window: usize,
) -> std::io::Result<(u64, u64, Vec<u8>, bool)> {
    use std::io::Read;
    let mut buffer = [0u8; 65536 + 64];
    let (mut hash, mut previous_hash) = (0xcbf29ce484222325, 0xcbf29ce484222325);
    let (mut read, mut carry) = (0u64, 0usize);
    let mut bytes = Vec::new();
    let mut loss = false;
    while read < len {
        let n = (len - read).min(65536) as usize;
        file.read_exact(&mut buffer[carry..carry + n])?;
        let data = &buffer[carry..carry + n];
        hash = log_fingerprint(hash, data);
        let old_n = previous_len.saturating_sub(read).min(n as u64) as usize;
        previous_hash = log_fingerprint(previous_hash, &data[..old_n]);
        let from = read.max(offset);
        let to = (read + n as u64).min(offset.saturating_add(window as u64));
        if from < to {
            bytes.extend_from_slice(&data[(from - read) as usize..(to - read) as usize]);
        }
        let scanned = &buffer[..carry + n];
        loss |= [
            b"[qex] The output of this job reached the limit".as_slice(),
            b"[qex] ----".as_slice(),
        ]
        .iter()
        .any(|note| scanned.windows(note.len()).any(|w| w == *note));
        read += n as u64;
        let end = carry + n;
        carry = end.min(64);
        buffer.copy_within(end - carry..end, 0);
    }
    Ok((hash, previous_hash, bytes, loss))
}

struct LogSnapshot {
    position: LogPosition,
    bytes: Vec<u8>,
    state: &'static str,
    retention_loss: bool,
    dropped_bytes: u64,
    dropped_lines: u64,
    incomplete: bool,
}

fn log_snapshot(
    dir: &std::path::Path,
    name: &str,
    file: &str,
    old: Option<&LogPosition>,
    status: &crate::job::JobStatus,
    window: usize,
) -> LogSnapshot {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = dir.join(file);
    let dropped = status
        .logs_dropped
        .and_then(|d| d.of(name))
        .unwrap_or_default();
    let incomplete = status.logs_dropped.is_some_and(|d| d.incomplete);
    let mut snapshot = LogSnapshot {
        position: LogPosition {
            n: name.into(),
            m: None,
            o: old.map_or(0, |p| p.o),
        },
        bytes: Vec::new(),
        state: "ok",
        retention_loss: dropped.0 > 0 || incomplete || crate::logcap::tail_path(&path).exists(),
        dropped_bytes: dropped.0,
        dropped_lines: dropped.1,
        incomplete,
    };
    let read = (|| -> std::io::Result<_> {
        // A directory or FIFO is a read fault, never empty output or a blocking read.
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)?;
        let before = f.metadata()?;
        if !before.is_file() {
            return Err(std::io::Error::other("not a regular log file"));
        }
        let old_len = old.and_then(|p| p.m).map_or(0, |m| m.2);
        let (hash, previous_hash, bytes, loss) =
            scan_log(&mut f, before.len(), old_len, snapshot.position.o, window)?;
        // Check the same prefix twice. Concurrent appends are allowed; a
        // retention rewrite during the read is an explicit unstable snapshot.
        f.seek(SeekFrom::Start(0))?;
        let (check, _, _, _) = scan_log(&mut f, before.len(), 0, 0, 0)?;
        let after = std::fs::metadata(&path)?;
        let stable = hash == check
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && after.len() >= before.len();
        Ok((before, bytes, hash, previous_hash, loss, stable))
    })();
    match read {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            snapshot.state = if old.is_some_and(|p| p.m.is_some()) || status.state.is_terminal() {
                "missing"
            } else {
                "waiting"
            };
        }
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            snapshot.state = "changed_during_read"
        }
        Err(_) => snapshot.state = "read_error",
        Ok((metadata, bytes, hash, previous_hash, loss, stable)) => {
            let mark = (metadata.dev(), metadata.ino(), metadata.len(), hash);
            snapshot.position.m = Some(mark);
            snapshot.retention_loss |= loss;
            snapshot.state = if !stable {
                "changed_during_read"
            } else if let Some((dev, ino, len, hash)) = old.and_then(|p| p.m) {
                if dev != mark.0 || ino != mark.1 {
                    "replaced"
                } else if mark.2 < len {
                    "shrunk"
                } else if previous_hash != hash {
                    "rewritten"
                } else {
                    "ok"
                }
            } else {
                "ok"
            };
            snapshot.bytes = bytes;
        }
    }
    snapshot
}

/// Convert only complete original byte units that fit. An incomplete UTF-8
/// suffix waits for a later append, except in a terminal job. Invalid units
/// use the same replacement character as the legacy log reader.
fn log_prefix(bytes: &[u8], limit: usize, terminal: bool) -> (String, usize) {
    let mut text = String::new();
    let mut used = 0;
    while used < bytes.len() && used < limit {
        let (valid, invalid) = match std::str::from_utf8(&bytes[used..]) {
            Ok(s) => (s, None),
            Err(e) => (
                std::str::from_utf8(&bytes[used..used + e.valid_up_to()]).unwrap(),
                Some(e),
            ),
        };
        let mut take = valid.len().min(limit - used);
        while !valid.is_char_boundary(take) {
            take -= 1;
        }
        text.push_str(&valid[..take]);
        used += take;
        if take < valid.len() {
            break;
        }
        let Some(error) = invalid else { break };
        let bad = match error.error_len() {
            Some(n) => n,
            None if terminal => bytes.len() - used,
            None => break,
        };
        if bad > limit - used {
            break;
        }
        text.push('\u{fffd}');
        used += bad;
    }
    (text, used)
}

/// Opt-in bounded incremental JSON. Legacy line selectors use `apply` above.
pub fn incremental(
    dir: &std::path::Path,
    id: uuid::Uuid,
    streams: &[(&str, &str)],
    raw_cursor: Option<&str>,
    cap: usize,
    status: &crate::job::JobStatus,
) -> anyhow::Result<(String, bool)> {
    use serde_json::json;
    let old = raw_cursor.and_then(LogCursor::decode);
    let error = if raw_cursor.is_some() && old.is_none() {
        Some("invalid_cursor")
    } else if old.as_ref().is_some_and(|c| c.j != id) {
        Some("foreign_cursor")
    } else if old.as_ref().is_some_and(|c| {
        c.s.iter()
            .map(|p| p.n.as_str())
            .ne(streams.iter().map(|s| s.0))
    }) {
        Some("selection_changed")
    } else {
        None
    };
    if let Some(error) = error {
        let page =
            json!({"version":1,"id":id,"state":"error","error":error,"streams":{},"cursor":null});
        return Ok((serde_json::to_string(&page)?, true));
    }
    let snapshots: Vec<_> = streams
        .iter()
        .enumerate()
        .map(|(i, (name, file))| {
            log_snapshot(
                dir,
                name,
                file,
                old.as_ref().map(|c| &c.s[i]),
                status,
                cap.saturating_add(3),
            )
        })
        .collect();
    let failed = snapshots
        .iter()
        .any(|s| !matches!(s.state, "ok" | "waiting"));
    let loss = snapshots.iter().any(|s| s.retention_loss);
    let render = |mut take: usize| -> anyhow::Result<String> {
        let mut next = LogCursor {
            v: 1,
            j: id,
            s: Vec::new(),
        };
        let mut output = serde_json::Map::new();
        for snapshot in &snapshots {
            let mut p = snapshot.position.clone();
            let (text, used) = if failed {
                (String::new(), 0)
            } else {
                log_prefix(
                    &snapshot.bytes,
                    take,
                    status.state.is_terminal()
                        && snapshot
                            .position
                            .m
                            .is_some_and(|m| m.2 - p.o == snapshot.bytes.len() as u64),
                )
            };
            let start = p.o;
            p.o += used as u64;
            take -= used;
            let remaining = &snapshot.bytes[used..];
            let has_more = snapshot.position.m.is_some_and(|m| m.2 > p.o);
            let pending_utf8 = !status.state.is_terminal()
                && !remaining.is_empty()
                && snapshot
                    .position
                    .m
                    .is_some_and(|m| m.2 - p.o == remaining.len() as u64)
                && std::str::from_utf8(remaining)
                    .is_err_and(|e| e.valid_up_to() == 0 && e.error_len().is_none());
            output.insert(p.n.clone(), json!({
                "text":text, "state":snapshot.state, "start":start, "end":p.o,
                "has_more":has_more, "pending_utf8":pending_utf8,
                "retention_loss":snapshot.retention_loss, "dropped_bytes":snapshot.dropped_bytes,
                "dropped_lines":snapshot.dropped_lines, "incomplete":snapshot.incomplete,
            }));
            next.s.push(p);
        }
        Ok(serde_json::to_string(&json!({
            "version":1, "id":id, "state":if failed { "error" } else if loss { "loss" } else { "ok" },
            "streams":output, "cursor":if failed { None } else { Some(next.encode()) },
        }))?)
    };
    if render(0)?.len() + 1 > cap {
        anyhow::bail!("--max-bytes {cap} cannot hold incremental log metadata; increase the cap");
    }
    let (mut lo, mut hi) = (0, cap);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if render(mid)?.len() < cap {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let page = render(lo)?;
    // A tiny cap must not trap the caller at a reusable but immobile cursor.
    if !failed
        && snapshots
            .iter()
            .any(|s| s.position.m.is_some_and(|m| m.2 > s.position.o))
    {
        let result: serde_json::Value = serde_json::from_str(&page)?;
        let advanced = result["streams"]
            .as_object()
            .unwrap()
            .values()
            .any(|s| s["start"] != s["end"]);
        let pending = result["streams"]
            .as_object()
            .unwrap()
            .values()
            .any(|s| s["pending_utf8"] == true);
        if !advanced && !pending {
            anyhow::bail!("--max-bytes {cap} cannot hold the next log unit; increase the cap");
        }
    }
    Ok((page, failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_scan_keeps_only_the_window_and_checks_the_whole_prefix() {
        use std::io::{Seek, SeekFrom};
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let path =
            Fixture(std::env::temp_dir().join(format!("qex-c-scan-{}", uuid::Uuid::new_v4())));
        let mut raw = vec![b'a'; 150000];
        let note = b"[qex] The output of this job reached the limit";
        raw[65520..65520 + note.len()].copy_from_slice(note); // Cross a scan boundary.
        std::fs::write(&path.0, &raw).unwrap();
        let mut file = std::fs::File::open(&path.0).unwrap();
        let (hash, old, window, loss) =
            scan_log(&mut file, raw.len() as u64, 70000, 120, 128).unwrap();
        assert_eq!(window, raw[120..248]);
        assert!(loss);
        assert_eq!(hash, log_fingerprint(0xcbf29ce484222325, &raw));
        assert_eq!(old, log_fingerprint(0xcbf29ce484222325, &raw[..70000]));
        file.seek(SeekFrom::Start(0)).unwrap();
        let (second, _, empty, _) = scan_log(&mut file, raw.len() as u64, 0, 0, 0).unwrap();
        assert_eq!(second, hash);
        assert!(empty.is_empty());
        raw[69999] = b'z'; // A rewrite outside the retained candidate window.
        std::fs::write(&path.0, &raw).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let (_, changed, window, _) =
            scan_log(&mut file, raw.len() as u64, 70000, 120, 128).unwrap();
        assert_eq!(window.len(), 128);
        assert_ne!(changed, old);
    }

    #[test]
    fn incremental_prefix_counts_original_bytes_at_every_boundary() {
        let bytes = b"a\"\\\n\0\xe2\x82\xac\xf0\x9f\x98\x80\xff\xe2\x82";
        for terminal in [false, true] {
            for cap in 0..=bytes.len() {
                let (text, used) = log_prefix(bytes, cap, terminal);
                assert!(used <= cap);
                assert_eq!(text, String::from_utf8_lossy(&bytes[..used]));
                if !terminal {
                    assert!(used <= bytes.len() - 2);
                }
            }
        }
        assert_eq!(log_prefix(b"\xe2\x82", 2, false), (String::new(), 0));
        assert_eq!(log_prefix(b"\xe2\x82", 2, true), ("\u{fffd}".into(), 2));
        assert_eq!(log_prefix(b"\xe2\x82\xac", 2, false), (String::new(), 0));
        assert_eq!(log_prefix(b"\xe2\x82\xac", 3, false), ("€".into(), 3));
    }

    #[test]
    fn incremental_cursor_rejects_malformed_versions_and_offsets() {
        for raw in ["", "qlog1-0", "qlog1-zz", "qlog2-00", "qlog1-c3a9"] {
            assert!(LogCursor::decode(raw).is_none(), "{raw}");
        }
        let mut c = LogCursor {
            v: 1,
            j: uuid::Uuid::new_v4(),
            s: vec![LogPosition {
                n: "stdout".into(),
                m: Some((1, 2, 3, 4)),
                o: 3,
            }],
        };
        assert_eq!(LogCursor::decode(&c.encode()).unwrap().s[0].o, 3);
        c.s[0].o = 4;
        assert!(LogCursor::decode(&c.encode()).is_none());
        c.s[0].o = 0;
        c.v = 2;
        assert!(LogCursor::decode(&c.encode()).is_none());
    }

    fn sel() -> LogSelect {
        LogSelect::default()
    }

    fn text() -> String {
        (1..=1000)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn with_no_option_the_last_lines_come_back() {
        let out = sel().apply(&text(), 500).unwrap();
        assert_eq!(out.text.lines().count(), 500);
        assert!(out.text.contains("line-1000"));
        assert!(
            !out.text.contains("line-1\n"),
            "the first line must be hidden"
        );
        assert!(out.truncated);
        assert_eq!(out.hidden, 500);
    }

    #[test]
    fn head_gives_the_start_and_tail_gives_the_end() {
        let mut s = sel();
        s.head = Some(3);
        let out = s.apply(&text(), 500).unwrap();
        assert_eq!(out.text.lines().next().unwrap(), "line-1");
        assert_eq!(out.text.lines().count(), 3);

        let mut s = sel();
        s.tail = Some(3);
        let out = s.apply(&text(), 500).unwrap();
        assert_eq!(out.text.lines().last().unwrap(), "line-1000");
        assert_eq!(out.text.lines().count(), 3);
    }

    #[test]
    fn a_range_gives_the_lines_between_two_numbers() {
        let mut s = sel();
        s.lines = Some("10:12".into());
        let out = s.apply(&text(), 500).unwrap();
        assert_eq!(out.text, "line-10\nline-11\nline-12\n");
    }

    #[test]
    fn a_range_with_an_incorrect_form_gives_a_clear_error() {
        let mut s = sel();
        s.lines = Some("10".into());
        assert!(s.apply(&text(), 500).unwrap_err().contains("N:M"));

        s.lines = Some("50:10".into());
        assert!(s.apply(&text(), 500).unwrap_err().contains("after"));

        s.lines = Some("0:5".into());
        assert!(s.apply(&text(), 500).unwrap_err().contains("starts at 1"));
    }

    #[test]
    fn all_gives_every_line() {
        let mut s = sel();
        s.all = true;
        let out = s.apply(&text(), 500).unwrap();
        assert_eq!(out.text.lines().count(), 1000);
        assert!(!out.truncated);
    }

    #[test]
    fn the_line_numbers_appear_with_the_option() {
        let mut s = sel();
        s.tail = Some(1);
        s.number = true;
        let out = s.apply(&text(), 500).unwrap();
        assert!(out.text.contains("1000"), "got: {}", out.text);
    }

    #[test]
    fn a_search_gives_the_lines_that_match() {
        let body = "alpha ok\nbeta FAIL here\ngamma ok\ndelta FAIL too\n";
        let mut s = sel();
        s.grep = Some("FAIL".into());
        let out = s.apply(body, 500).unwrap();
        // The output holds a mark where the selection leaves a gap, so count
        // the log lines only.
        let shown: Vec<&str> = out.text.lines().filter(|l| *l != "--").collect();
        assert_eq!(shown, vec!["beta FAIL here", "delta FAIL too"]);
        assert_eq!(out.matches, Some(2));
    }

    /// A match alone is frequently not enough. The lines after a failure hold
    /// the reason for it.
    #[test]
    fn the_context_option_gives_the_lines_around_a_match() {
        let body = "a\nb\nFAIL\nc\nd\ne\n";
        let mut s = sel();
        s.grep = Some("FAIL".into());
        s.context = Some(1);
        let out = s.apply(body, 500).unwrap();
        assert_eq!(out.text, "b\nFAIL\nc\n");
    }

    /// A gap in the selection must be visible, or a reader joins two lines that
    /// are far apart.
    #[test]
    fn a_gap_between_matches_is_marked() {
        let body = "FAIL one\nx\nx\nx\nx\nFAIL two\n";
        let mut s = sel();
        s.grep = Some("FAIL".into());
        let out = s.apply(body, 500).unwrap();
        assert!(out.text.contains("--\n"), "got: {}", out.text);
    }

    #[test]
    fn the_search_reads_a_small_pattern_language() {
        let body = "error: one\nwarning: two\nERROR: three\nnote: four\n";

        let mut s = sel();
        s.grep = Some("error|warning".into());
        assert_eq!(s.apply(body, 500).unwrap().text.lines().count(), 2);

        let mut s = sel();
        s.grep = Some("^note".into());
        assert_eq!(s.apply(body, 500).unwrap().text.trim(), "note: four");

        let mut s = sel();
        s.grep = Some("e[rn]ror".into());
        assert_eq!(s.apply(body, 500).unwrap().text.lines().count(), 1);

        let mut s = sel();
        s.grep = Some("t.*ee$".into());
        assert_eq!(s.apply(body, 500).unwrap().text.trim(), "ERROR: three");
    }

    #[test]
    fn the_fixed_option_reads_the_value_as_plain_text() {
        let body = "cost is 3.50\ncost is 3x50\n";
        let mut s = sel();
        s.grep = Some("3.50".into());
        // As a pattern, the dot matches any character.
        assert_eq!(s.apply(body, 500).unwrap().text.lines().count(), 2);

        s.fixed = true;
        assert_eq!(s.apply(body, 500).unwrap().text.trim(), "cost is 3.50");
    }

    /// A pattern that this search cannot read must give a clear error, and it
    /// must name the option that always operates.
    #[test]
    fn an_unsupported_pattern_names_the_alternative() {
        let mut s = sel();
        s.grep = Some("(a|b)c".into());
        let err = s.apply("abc", 500).unwrap_err();
        assert!(err.contains("--fixed"), "got: {err}");
    }

    /// A search that matches every line must not give the whole file to a
    /// reader who gave no other option, and it must say how many lines matched.
    #[test]
    fn a_search_that_matches_everything_still_has_a_limit() {
        let mut s = sel();
        s.grep = Some("line".into());
        let out = s.apply(&text(), 500).unwrap();

        assert_eq!(out.matches, Some(1000), "the total must be the true total");
        assert_eq!(out.matches_shown, DEFAULT_MATCHES);
        assert_eq!(out.text.lines().count(), DEFAULT_MATCHES);
    }

    /// The limit keeps the FIRST matches. In a log, the first failure is
    /// usually the cause, and the later failures are the result of it.
    #[test]
    fn the_match_limit_keeps_the_first_matches() {
        let mut s = sel();
        s.grep = Some("line".into());
        s.max_matches = Some(3);
        let out = s.apply(&text(), 500).unwrap();
        assert_eq!(out.matches, Some(1000));
        assert_eq!(out.matches_shown, 3);
        assert_eq!(out.text, "line-1\nline-2\nline-3\n");
    }

    /// The context lines multiply the output, so the limit counts the matches
    /// and not the lines. Forty matches with three context lines are 280 lines.
    #[test]
    fn the_limit_counts_the_matches_and_not_the_lines() {
        let body: String = (1..=100)
            .map(|i| format!("pad-{i}\nFAIL-{i}\npad2-{i}\n"))
            .collect();
        let mut s = sel();
        s.grep = Some("FAIL".into());
        s.context = Some(1);
        s.max_matches = Some(5);
        let out = s.apply(&body, 500).unwrap();
        assert_eq!(out.matches, Some(100));
        assert_eq!(out.matches_shown, 5);
        // Five matches, and one line before and after each one.
        assert_eq!(
            out.text.lines().filter(|l| l.starts_with("FAIL")).count(),
            5
        );
    }

    /// The option `--all` removes the match limit as well.
    #[test]
    fn all_removes_the_match_limit() {
        let mut s = sel();
        s.grep = Some("line".into());
        s.all = true;
        let out = s.apply(&text(), 500).unwrap();
        assert_eq!(out.matches, Some(1000));
        assert_eq!(out.matches_shown, 1000);
    }
}
