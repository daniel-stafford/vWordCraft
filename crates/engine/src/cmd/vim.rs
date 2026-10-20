//! Vim keys: an opt-in modal layer (Normal, Insert, Visual and Visual Line modes) over the
//! caret, selection and editing commands, for people whose hands know Vim. File › Options ›
//! Advanced or `vim.toggle` turns it on; the front end then sends the keys typed in Normal and
//! Visual mode to `vim.keys`, and types as usual in Insert mode until Escape.
//!
//! A Vim line is a paragraph: `dd`, `yy`, `cc`, `V`, `0`, `$`, `J` and `>>` take whole
//! paragraphs, as Vim does with a wrapped line. `j` and `k` move by displayed line (Vim's `gj`
//! and `gk`), which is what prose needs; as an operator's motion (`dj`) they take paragraphs.
//! The caret sits just before the character Vim's cursor would be on. Yanks and deletes go to
//! WordCraft's own clipboard (Vim's unnamed register), not the system one. Each edit is its own
//! undo step, as the commands it runs are.

use serde_json::{Value, json};
use wordcraft_doc::edit::Fragment;
use wordcraft_doc::{Block, Paragraph, Path, Pos};

use crate::{CmdError, CmdResult, CommandSpec, Selection, Session, p};

/// Most keys one call takes.
const MAX_KEYS: usize = 4096;
/// Largest count a command takes.
const MAX_COUNT: usize = 10_000;
/// Longest unfinished command kept (a count with an operator and a text object is a few keys).
const MAX_PENDING: usize = 24;
/// Displayed lines `<C-d>` and `<C-u>` move.
const HALF_PAGE_LINES: usize = 15;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("vim.toggle", "Vim Keys", "File › Options › Advanced", toggle).params(r#"{"value"?: bool (default: toggle)}"#).pure(),
        CommandSpec::new("vim.keys", "Vim Keystrokes", "Editing", keys)
            .params(
                r#"{"keys": string} — Vim notation: characters, <Esc>, <CR>, <BS>, <Del>, <Left> <Right> <Up> <Down>, <C-r> <C-d> <C-u> <C-f> <C-b>, <lt> for "<". Returns the mode ("normal|insert|visual|visualLine"), any unfinished command and whether the last key meant anything."#,
            )
            .when(|s| (!s.prefs.vim_keys).then_some("Vim keys are off (vim.toggle turns them on)"))
            .pure(),
    ]
}

/// The mode Vim keys are in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Insert => "insert",
            Mode::Visual => "visual",
            Mode::VisualLine => "visualLine",
        }
    }
}

/// A key as Vim names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Ctrl(char),
}

/// Vim keys' state in a session.
#[derive(Clone, Debug, Default)]
pub struct Vim {
    pub mode: Mode,
    /// Keys of an unfinished command (`2d`, `g`, `f`, `di`).
    pending: Vec<Key>,
    /// Whole paragraphs the last yank or delete took (`yy`, `dd`, `V` then `y`), which `p` puts
    /// below the caret's paragraph, with the clipboard text they left (another copy replaces
    /// them).
    lines: Option<(Vec<Block>, String)>,
    /// Visual mode's two ends.
    visual: Option<Visual>,
    /// The last `f`, `F`, `t` or `T`, for `;` and `,`.
    last_find: Option<(char, char)>,
}

#[derive(Clone, Debug)]
struct Visual {
    /// Where Visual mode started.
    anchor: Pos,
    /// Vim's cursor: the moving end, on a character.
    cursor: Pos,
    /// The selection shown, to notice a click that changed it.
    shown: Selection,
}

impl Vim {
    /// The unfinished command, as typed.
    pub fn pending(&self) -> String {
        self.pending.iter().map(|k| key_text(*k)).collect()
    }
}

fn key_text(k: Key) -> String {
    match k {
        Key::Char('<') => "<lt>".into(),
        Key::Char(c) => c.to_string(),
        Key::Esc => "<Esc>".into(),
        Key::Enter => "<CR>".into(),
        Key::Backspace => "<BS>".into(),
        Key::Delete => "<Del>".into(),
        Key::Left => "<Left>".into(),
        Key::Right => "<Right>".into(),
        Key::Up => "<Up>".into(),
        Key::Down => "<Down>".into(),
        Key::Ctrl(c) => format!("<C-{c}>"),
    }
}

/// Keys in Vim notation: characters, and `<Esc>`, `<CR>`, `<C-r>`… for the others.
pub fn parse_keys(text: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Some(k) = rest.get(1..end).and_then(named_key)
        {
            out.push(k);
            rest = rest.get(end + 1..).unwrap_or("");
            continue;
        }
        out.push(Key::Char(c));
        rest = rest.get(c.len_utf8()..).unwrap_or("");
    }
    out
}

fn named_key(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "esc" => Key::Esc,
        "cr" | "enter" | "return" => Key::Enter,
        "bs" => Key::Backspace,
        "del" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "lt" => Key::Char('<'),
        "space" => Key::Char(' '),
        "tab" => Key::Char('\t'),
        _ => {
            let mut c = lower.strip_prefix("c-")?.chars();
            let k = c.next()?;
            if c.next().is_some() {
                return None;
            }
            Key::Ctrl(k)
        }
    })
}

fn toggle(s: &mut Session, v: &Value) -> CmdResult {
    s.prefs.vim_keys = p::bool(v, "value").unwrap_or(!s.prefs.vim_keys);
    s.vim = Vim::default();
    if s.prefs.vim_keys {
        settle(s);
    }
    Ok(state(s, true))
}

fn keys(s: &mut Session, v: &Value) -> CmdResult {
    let keys = parse_keys(p::req_str(v, "keys")?);
    if keys.len() > MAX_KEYS {
        return Err(CmdError::Params(format!("`keys` takes at most {MAX_KEYS} keys")));
    }
    let mut handled = true;
    for k in keys {
        handled = feed(s, k);
    }
    Ok(state(s, handled))
}

/// What `vim.toggle` and `vim.keys` return.
pub fn state(s: &Session, handled: bool) -> Value {
    json!({"value": s.prefs.vim_keys, "mode": s.vim.mode.name(), "pending": s.vim.pending(), "handled": handled})
}

/// One key. False when it meant nothing here, so the window may use it (Escape in Normal mode
/// with nothing to cancel: it still collapses a selection or leaves a header).
pub fn feed(s: &mut Session, k: Key) -> bool {
    sync(s);
    match s.vim.mode {
        Mode::Insert => insert_key(s, k),
        Mode::Normal => normal_key(s, k),
        Mode::Visual | Mode::VisualLine => visual_key(s, k),
    }
}

/// Catch up with what the mouse or other commands did: a selection made in Normal mode is
/// Visual mode's, and a click in Visual mode ends it.
fn sync(s: &mut Session) {
    match s.vim.mode {
        Mode::Normal if !s.sel.is_collapsed() => {
            s.vim.pending.clear();
            let (anchor, focus) = (s.sel.anchor.clone(), s.sel.focus.clone());
            let cursor = if focus > anchor { prev_char(s, &focus) } else { focus };
            start_visual(s, Mode::Visual, anchor, cursor);
        }
        Mode::Visual | Mode::VisualLine if s.vim.visual.as_ref().is_none_or(|v| v.shown != s.sel) => {
            if s.sel.is_collapsed() {
                s.vim.mode = Mode::Normal;
                s.vim.visual = None;
                s.vim.pending.clear();
            } else {
                let (anchor, focus) = (s.sel.anchor.clone(), s.sel.focus.clone());
                let cursor = if focus > anchor { prev_char(s, &focus) } else { focus };
                start_visual(s, s.vim.mode, anchor, cursor);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Positions

fn para_len(s: &Session, p: &Pos) -> usize {
    s.doc.para_at(p).map_or(0, |x| x.len())
}

fn char_at(s: &Session, p: &Pos) -> Option<char> {
    s.doc.para_at(p).and_then(|x| x.text.get(p.off..)).and_then(|t| t.chars().next())
}

fn at(p: &Pos, off: usize) -> Pos {
    Pos { off, ..p.clone() }
}

/// The caret stop after `p` in its paragraph (its end stays).
fn next_char(s: &Session, p: &Pos) -> Pos {
    match s.doc.para_at(p) {
        Some(x) if p.off < x.len() => at(p, x.next_boundary(p.off)),
        _ => p.clone(),
    }
}

/// The caret stop before `p` in its paragraph (its start stays).
fn prev_char(s: &Session, p: &Pos) -> Pos {
    match s.doc.para_at(p) {
        Some(x) if p.off > 0 => at(p, x.prev_boundary(p.off)),
        _ => p.clone(),
    }
}

/// `n` caret stops forward in the paragraph, stopping at its end.
fn chars_fwd(s: &Session, p: &Pos, n: usize) -> Pos {
    let mut q = p.clone();
    for _ in 0..n {
        let r = next_char(s, &q);
        if r == q {
            break;
        }
        q = r;
    }
    q
}

/// `n` caret stops back in the paragraph, stopping at its start.
fn chars_back(s: &Session, p: &Pos, n: usize) -> Pos {
    let mut q = p.clone();
    for _ in 0..n {
        let r = prev_char(s, &q);
        if r == q {
            break;
        }
        q = r;
    }
    q
}

/// The next position in the story, across paragraph ends (a paragraph's end is its mark).
fn step_fwd(s: &Session, p: &Pos) -> Option<Pos> {
    if p.off < para_len(s, p) {
        return Some(next_char(s, p));
    }
    s.doc.next_para(p.story, &p.path).map(|q| Pos { story: p.story, path: q, off: 0 })
}

fn step_back(s: &Session, p: &Pos) -> Option<Pos> {
    if p.off > 0 {
        return Some(prev_char(s, p));
    }
    let q = s.doc.prev_para(p.story, &p.path)?;
    let off = s.doc.para(p.story, &q).map_or(0, |x| x.len());
    Some(Pos { story: p.story, path: q, off })
}

/// The paragraph after or before `path` in the same container (body, cell, text box).
fn sibling(s: &Session, p: &Pos, fwd: bool) -> Option<Path> {
    let q = if fwd { s.doc.next_para(p.story, &p.path) } else { s.doc.prev_para(p.story, &p.path) }?;
    (q.parent() == p.path.parent()).then_some(q)
}

/// `n` paragraphs on in the same container, as far as there are.
fn paras_on(s: &Session, p: &Pos, n: usize, fwd: bool) -> Pos {
    let mut q = at(p, 0);
    for _ in 0..n {
        match sibling(s, &q, fwd) {
            Some(path) => q.path = path,
            None => break,
        }
    }
    q
}

fn first_non_blank(s: &Session, p: &Pos) -> Pos {
    let text = s.doc.para_at(p).map_or("", |x| x.text.as_str());
    let off = text.char_indices().find(|(_, c)| !c.is_whitespace()).map_or(text.len(), |(i, _)| i);
    at(p, off)
}

fn end_of_para(s: &Session, p: &Pos) -> Pos {
    at(p, para_len(s, p))
}

/// Vim's character classes: blank (a paragraph's end too), word, punctuation. With `big` every
/// non-blank is a word (Vim's WORD).
fn class(c: Option<char>, big: bool) -> u8 {
    match c {
        None => 0,
        Some(c) if c.is_whitespace() || c == '\u{a0}' => 0,
        Some(_) if big => 1,
        Some(c) if wordcraft_doc::bidi::is_word_char(c) => 1,
        Some(_) => 2,
    }
}

fn class_at(s: &Session, p: &Pos, big: bool) -> u8 {
    class(char_at(s, p), big)
}

fn empty_para(s: &Session, p: &Pos) -> bool {
    p.off == 0 && para_len(s, p) == 0
}

/// `w`: the start of the next word. An empty paragraph counts as a word, as an empty line does.
fn word_fwd(s: &Session, p: &Pos, big: bool) -> Pos {
    let c0 = class_at(s, p, big);
    let mut q = p.clone();
    if c0 != 0 {
        while let Some(r) = step_fwd(s, &q) {
            q = r;
            if class_at(s, &q, big) != c0 || q.off == 0 {
                break;
            }
        }
    }
    while class_at(s, &q, big) == 0 {
        if q != *p && empty_para(s, &q) {
            break;
        }
        match step_fwd(s, &q) {
            Some(r) => q = r,
            None => break,
        }
    }
    q
}

/// `e`: the last character of this or the next word.
fn word_end(s: &Session, p: &Pos, big: bool) -> Pos {
    let Some(mut q) = step_fwd(s, p) else { return p.clone() };
    while class_at(s, &q, big) == 0 {
        match step_fwd(s, &q) {
            Some(r) => q = r,
            None => return q,
        }
    }
    let c = class_at(s, &q, big);
    loop {
        let r = next_char(s, &q);
        if r == q || r.off >= para_len(s, &r) || class_at(s, &r, big) != c {
            return q;
        }
        q = r;
    }
}

/// `b`: the start of this or the previous word.
fn word_back(s: &Session, p: &Pos, big: bool) -> Pos {
    let Some(mut q) = step_back(s, p) else { return p.clone() };
    while class_at(s, &q, big) == 0 {
        if empty_para(s, &q) {
            return q;
        }
        match step_back(s, &q) {
            Some(r) => q = r,
            None => return q,
        }
    }
    let c = class_at(s, &q, big);
    while q.off > 0 {
        let r = prev_char(s, &q);
        if class_at(s, &r, big) != c {
            break;
        }
        q = r;
    }
    q
}

/// `)`: the start of the next sentence (a paragraph's start is one).
fn sentence_fwd(s: &Session, p: &Pos) -> Pos {
    let Some(para) = s.doc.para_at(p) else { return p.clone() };
    let (_, end) = para.sentence_at(p.off);
    if end > p.off && end < para.len() {
        return at(p, end);
    }
    match s.doc.next_para(p.story, &p.path) {
        Some(q) => Pos { story: p.story, path: q, off: 0 },
        None => end_of_para(s, p),
    }
}

/// `(`: the start of this sentence, or the previous one when already there.
fn sentence_back(s: &Session, p: &Pos) -> Pos {
    let Some(para) = s.doc.para_at(p) else { return p.clone() };
    let (start, _) = para.sentence_at(p.off);
    if start < p.off {
        return at(p, start);
    }
    if p.off > 0 {
        return at(p, para.sentence_at(para.prev_boundary(p.off)).0);
    }
    match step_back(s, p) {
        Some(q) => {
            let start = s.doc.para_at(&q).map_or(0, |x| x.sentence_at(x.len()).0);
            at(&q, start)
        }
        None => p.clone(),
    }
}

/// `f`, `F`, `t` or `T` for `ch`, `n` times, within the paragraph.
fn find_char(s: &Session, p: &Pos, kind: char, ch: char, n: usize) -> Option<Pos> {
    let para = s.doc.para_at(p)?;
    let text = para.text.as_str();
    let fwd = matches!(kind, 'f' | 't');
    let mut found = None;
    let mut left = n.max(1);
    if fwd {
        // `t` right before its character would find the same one again on `;`: look past it.
        let from = if kind == 't' { para.next_boundary(p.off) } else { p.off };
        for (i, c) in text.get(from..)?.char_indices() {
            let off = from + i;
            if off > p.off && c == ch {
                left -= 1;
                if left == 0 {
                    found = Some(off);
                    break;
                }
            }
        }
    } else {
        let to = if kind == 'T' { para.prev_boundary(p.off) } else { p.off };
        for (off, c) in text.get(..to)?.char_indices().rev() {
            if c == ch {
                left -= 1;
                if left == 0 {
                    found = Some(off);
                    break;
                }
            }
        }
    }
    let off = found?;
    Some(match kind {
        't' => at(p, para.prev_boundary(off)),
        'T' => at(p, para.next_boundary(off)),
        _ => at(p, off),
    })
}

// ---------------------------------------------------------------------------------------------
// Parsing

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordFwd(bool),
    WordBack(bool),
    WordEnd(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    NextLine,
    PrevLine,
    DisplayStart,
    DisplayEnd,
    DocStart,
    DocEnd,
    ParaFwd,
    ParaBack,
    SentenceFwd,
    SentenceBack,
    Find(char, char),
    RepeatFind(bool),
}

/// How an operator takes a motion's span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Motion(Motion),
    /// `iw`, `aW`, `is`, `ap`…: (around, object).
    Object(bool, char),
    /// The operator doubled (`dd`, `yy`, `>>`).
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Normal {
    Move(Motion),
    Op(char, Target),
    Simple(char),
    Replace(char),
    Ctrl(char),
}

enum Parse<T> {
    More,
    Bad,
    Done(T),
}

struct Reader<'a> {
    keys: &'a [Key],
    i: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<Key> {
        self.keys.get(self.i).copied()
    }
    fn next(&mut self) -> Option<Key> {
        let k = self.peek();
        self.i += 1;
        k
    }
    /// A count (digits not starting with 0), if one is next.
    fn count(&mut self) -> Option<usize> {
        let mut n: Option<usize> = None;
        while let Some(Key::Char(c)) = self.peek() {
            let Some(d) = c.to_digit(10) else { break };
            if n.is_none() && d == 0 {
                break;
            }
            n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(d as usize).min(MAX_COUNT));
            self.i += 1;
        }
        n
    }
}

fn motion(r: &mut Reader) -> Parse<Motion> {
    use Motion as M;
    let Some(k) = r.next() else { return Parse::More };
    let m = match k {
        Key::Char('h') | Key::Left | Key::Backspace => M::Left,
        Key::Char('l' | ' ') | Key::Right => M::Right,
        Key::Char('j') | Key::Down => M::Down,
        Key::Char('k') | Key::Up => M::Up,
        Key::Char('w') => M::WordFwd(false),
        Key::Char('W') => M::WordFwd(true),
        Key::Char('b') => M::WordBack(false),
        Key::Char('B') => M::WordBack(true),
        Key::Char('e') => M::WordEnd(false),
        Key::Char('E') => M::WordEnd(true),
        Key::Char('0') => M::LineStart,
        Key::Char('^') => M::FirstNonBlank,
        Key::Char('$') => M::LineEnd,
        Key::Char('+') | Key::Enter => M::NextLine,
        Key::Char('-') => M::PrevLine,
        Key::Char('G') => M::DocEnd,
        Key::Char('}') => M::ParaFwd,
        Key::Char('{') => M::ParaBack,
        Key::Char(')') => M::SentenceFwd,
        Key::Char('(') => M::SentenceBack,
        Key::Char(';') => M::RepeatFind(false),
        Key::Char(',') => M::RepeatFind(true),
        Key::Char(kind @ ('f' | 'F' | 't' | 'T')) => match r.next() {
            None => return Parse::More,
            Some(Key::Char(c)) => M::Find(kind, c),
            Some(_) => return Parse::Bad,
        },
        Key::Char('g') => match r.next() {
            None => return Parse::More,
            Some(Key::Char('g')) => M::DocStart,
            Some(Key::Char('j')) => M::Down,
            Some(Key::Char('k')) => M::Up,
            Some(Key::Char('0')) => M::DisplayStart,
            Some(Key::Char('$')) => M::DisplayEnd,
            Some(_) => return Parse::Bad,
        },
        _ => return Parse::Bad,
    };
    Parse::Done(m)
}

/// A Normal mode command: its count (1 without one) and what it does.
fn parse_normal(keys: &[Key]) -> Parse<(Option<usize>, Normal)> {
    let mut r = Reader { keys, i: 0 };
    let c1 = r.count();
    let Some(k) = r.peek() else { return Parse::More };
    let cmd = match k {
        Key::Char(op @ ('d' | 'c' | 'y' | '>' | '<')) => {
            r.i += 1;
            let c2 = r.count();
            let count = match (c1, c2) {
                (Some(a), Some(b)) => Some(a.saturating_mul(b).min(MAX_COUNT)),
                (a, b) => a.or(b),
            };
            let target = match r.peek() {
                None => return Parse::More,
                Some(Key::Char(c)) if c == op => Target::Lines,
                Some(Key::Char(around @ ('i' | 'a'))) => {
                    r.i += 1;
                    match r.next() {
                        None => return Parse::More,
                        Some(Key::Char(o @ ('w' | 'W' | 's' | 'p'))) => Target::Object(around == 'a', o),
                        Some(_) => return Parse::Bad,
                    }
                }
                Some(_) => match motion(&mut r) {
                    Parse::Done(m) => Target::Motion(m),
                    Parse::More => return Parse::More,
                    Parse::Bad => return Parse::Bad,
                },
            };
            return Parse::Done((count, Normal::Op(op, target)));
        }
        Key::Char('r') => {
            r.i += 1;
            match r.next() {
                None => return Parse::More,
                Some(Key::Char(c)) if c != '\t' => Normal::Replace(c),
                Some(_) => return Parse::Bad,
            }
        }
        Key::Char(c) if "xXDCsSYpPJ~uiaIAoOvVnN*#/?".contains(c) => Normal::Simple(c),
        Key::Delete => Normal::Simple('x'),
        Key::Ctrl(c) => Normal::Ctrl(c),
        _ => match motion(&mut r) {
            Parse::Done(m) => Normal::Move(m),
            Parse::More => return Parse::More,
            Parse::Bad => return Parse::Bad,
        },
    };
    Parse::Done((c1, cmd))
}

// ---------------------------------------------------------------------------------------------
// Motions

/// Where `m` goes from `from`, `n` times, and how an operator takes it. `None` when it can't go
/// (no such character for `f`).
fn travel(s: &mut Session, from: &Pos, m: Motion, count: Option<usize>, op: bool) -> Option<(Pos, Kind)> {
    use Motion as M;
    let n = count.unwrap_or(1).max(1);
    let repeat = |s: &mut Session, f: &dyn Fn(&Session, &Pos) -> Pos| {
        let mut q = from.clone();
        for _ in 0..n {
            let r = f(s, &q);
            if r == q {
                break;
            }
            q = r;
        }
        q
    };
    if !matches!(m, M::Up | M::Down | M::DisplayStart | M::DisplayEnd) {
        s.goal_x = None;
    }
    Some(match m {
        M::Left => (chars_back(s, from, n), Kind::Exclusive),
        M::Right => (chars_fwd(s, from, n), Kind::Exclusive),
        M::Up | M::Down if op => (paras_on(s, from, n, m == M::Down), Kind::Linewise),
        M::Up | M::Down => (display_move(s, from, if m == M::Down { "caret.down" } else { "caret.up" }, n), Kind::Exclusive),
        M::WordFwd(big) => {
            let mut q = repeat(s, &|s, p| word_fwd(s, p, big));
            // `dw` on a paragraph's last word stops at its end rather than taking the mark.
            if op && q.path != from.path {
                q = end_of_para(s, from);
            }
            (q, Kind::Exclusive)
        }
        M::WordBack(big) => (repeat(s, &|s, p| word_back(s, p, big)), Kind::Exclusive),
        M::WordEnd(big) => (repeat(s, &|s, p| word_end(s, p, big)), Kind::Inclusive),
        M::LineStart => (at(from, 0), Kind::Exclusive),
        M::FirstNonBlank => (first_non_blank(s, from), Kind::Exclusive),
        M::LineEnd => (end_of_para(s, &paras_on(s, from, n - 1, true)), Kind::Inclusive),
        M::NextLine => (first_non_blank(s, &paras_on(s, from, n, true)), Kind::Linewise),
        M::PrevLine => (first_non_blank(s, &paras_on(s, from, n, false)), Kind::Linewise),
        M::DisplayStart => (display_move(s, from, "caret.home", 1), Kind::Exclusive),
        M::DisplayEnd => (display_move(s, from, "caret.end", 1), Kind::Exclusive),
        M::DocStart | M::DocEnd => {
            let paths = s.doc.para_paths(from.story);
            let path = match count {
                Some(k) => paths.get(k.saturating_sub(1)).or(paths.last()),
                None if m == M::DocStart => paths.first(),
                None => paths.last(),
            }?
            .clone();
            (first_non_blank(s, &Pos { story: from.story, path, off: 0 }), Kind::Linewise)
        }
        M::ParaFwd => (
            repeat(s, &|s, p| match s.doc.next_para(p.story, &p.path) {
                Some(q) => Pos { story: p.story, path: q, off: 0 },
                None => end_of_para(s, p),
            }),
            Kind::Exclusive,
        ),
        M::ParaBack => (
            repeat(s, &|s, p| {
                if p.off > 0 {
                    return at(p, 0);
                }
                match s.doc.prev_para(p.story, &p.path) {
                    Some(q) => Pos { story: p.story, path: q, off: 0 },
                    None => p.clone(),
                }
            }),
            Kind::Exclusive,
        ),
        M::SentenceFwd => (repeat(s, &sentence_fwd), Kind::Exclusive),
        M::SentenceBack => (repeat(s, &sentence_back), Kind::Exclusive),
        M::Find(kind, ch) => {
            s.vim.last_find = Some((kind, ch));
            find_motion(s, from, kind, ch, n)?
        }
        M::RepeatFind(reverse) => {
            let (kind, ch) = s.vim.last_find?;
            let kind = if reverse {
                match kind {
                    'f' => 'F',
                    'F' => 'f',
                    't' => 'T',
                    _ => 't',
                }
            } else {
                kind
            };
            find_motion(s, from, kind, ch, n)?
        }
    })
}

fn find_motion(s: &Session, from: &Pos, kind: char, ch: char, n: usize) -> Option<(Pos, Kind)> {
    let to = find_char(s, from, kind, ch, n)?;
    Some((to, if matches!(kind, 'f' | 't') { Kind::Inclusive } else { Kind::Exclusive }))
}

/// Move by displayed lines (or to a displayed line's ends) with the caret commands, which know
/// the layout and keep the column `j`/`k` aim for.
fn display_move(s: &mut Session, from: &Pos, id: &str, n: usize) -> Pos {
    let goal = s.goal_x;
    s.sel = Selection::caret(from.clone());
    s.goal_x = goal;
    for _ in 0..n {
        let before = s.sel.focus.clone();
        if s.run(id, &json!({})).is_err() || s.sel.focus == before {
            break;
        }
    }
    s.sel.focus.clone()
}

// ---------------------------------------------------------------------------------------------
// Ranges and operators

/// What an operator acts on.
#[derive(Clone, Debug, PartialEq)]
enum Span {
    /// Characters from the first position to the second.
    Chars(Pos, Pos),
    /// Whole paragraphs, the first to the last (same container).
    Lines(Pos, Pos),
}

fn motion_span(s: &Session, from: &Pos, to: &Pos, kind: Kind) -> Span {
    let (a, b) = if from <= to { (from.clone(), to.clone()) } else { (to.clone(), from.clone()) };
    match kind {
        Kind::Exclusive => Span::Chars(a, b),
        Kind::Inclusive => {
            let b = next_char(s, &b);
            Span::Chars(a, b)
        }
        Kind::Linewise => Span::Lines(at(&a, 0), at(&b, 0)),
    }
}

/// `iw`, `aw`, `is`, `as`, `ip`, `ap` at `p`.
fn object_span(s: &Session, p: &Pos, around: bool, obj: char, n: usize) -> Option<Span> {
    let para = s.doc.para_at(p)?;
    let text = para.text.as_str();
    match obj {
        'p' => Some(Span::Lines(at(p, 0), paras_on(s, p, n.saturating_sub(1), true))),
        's' => {
            let (a, b) = para.sentence_at(p.off);
            let b = if around { b } else { a + text.get(a..b)?.trim_end().len() };
            Some(Span::Chars(at(p, a), at(p, b)))
        }
        _ => {
            let big = obj == 'W';
            if text.is_empty() {
                return None;
            }
            let c = class_at(s, p, big);
            let mut a = p.clone();
            while a.off > 0 {
                let q = prev_char(s, &a);
                if class_at(s, &q, big) != c {
                    break;
                }
                a = q;
            }
            let mut b = p.clone();
            while b.off < text.len() && class_at(s, &b, big) == c {
                b = next_char(s, &b);
            }
            if around {
                // The blanks after the word, or before it when none follow.
                let mut e = b.clone();
                while e.off < text.len() && class_at(s, &e, big) == 0 && c != 0 {
                    e = next_char(s, &e);
                }
                if e == b && c != 0 {
                    while a.off > 0 {
                        let q = prev_char(s, &a);
                        if class_at(s, &q, big) != 0 {
                            break;
                        }
                        a = q;
                    }
                }
                b = e;
            }
            Some(Span::Chars(a, b))
        }
    }
}

/// A run of paragraphs as characters: from the first's start to the last's end, without the
/// last mark.
fn lines_chars(s: &Session, first: &Pos, last: &Pos) -> (Pos, Pos) {
    (at(first, 0), end_of_para(s, last))
}

/// The paragraphs of a span, to delete whole: with the mark before them when there is one in
/// the same container (the paragraph before keeps its formatting), else the one after them,
/// else just their text.
fn lines_delete_range(s: &Session, first: &Pos, last: &Pos) -> (Pos, Pos) {
    if let Some(prev) = sibling(s, first, false) {
        let prev = Pos { story: first.story, path: prev, off: 0 };
        return (end_of_para(s, &prev), end_of_para(s, last));
    }
    if let Some(next) = sibling(s, last, true) {
        return (at(first, 0), Pos { story: last.story, path: next, off: 0 });
    }
    lines_chars(s, first, last)
}

fn select(s: &mut Session, a: &Pos, b: &Pos) {
    s.sel = Selection { anchor: a.clone(), focus: b.clone() };
}

fn caret(s: &mut Session, p: Pos) {
    s.sel = Selection::caret(s.doc.clamp(&p));
}

/// Remember whole paragraphs for `p`, with the clipboard text the copy left.
fn keep_lines(s: &mut Session, first: &Pos, last: &Pos) {
    let (a, b) = lines_chars(s, first, last);
    let blocks = s.doc.copy_range(&a, &b).blocks;
    s.vim.lines = Some((blocks, String::new()));
}

fn note_clipboard(s: &mut Session) {
    let text = s.clipboard.as_ref().map(|f| f.clipboard_text()).unwrap_or_default();
    if let Some((_, t)) = s.vim.lines.as_mut() {
        *t = text;
    }
}

/// Run `op` (`d`, `c`, `y`, `>`, `<`) on `span`.
fn operate(s: &mut Session, op: char, span: Span) {
    match (op, span) {
        ('>' | '<', span) => {
            let (a, b) = match &span {
                Span::Chars(a, b) => (a.clone(), b.clone()),
                Span::Lines(f, l) => lines_chars(s, f, l),
            };
            select(s, &a, &b);
            let _ = s.run(if op == '>' { "para.indent" } else { "para.outdent" }, &json!({}));
            caret(s, first_non_blank(s, &a));
        }
        ('y', Span::Chars(a, b)) => {
            if a != b {
                select(s, &a, &b);
                let _ = s.run("edit.copy", &json!({}));
                s.vim.lines = None;
            }
            caret(s, a);
        }
        ('y', Span::Lines(f, l)) => {
            let (a, b) = lines_chars(s, &f, &l);
            keep_lines(s, &f, &l);
            select(s, &a, &b);
            let _ = s.run("edit.copy", &json!({}));
            note_clipboard(s);
            caret(s, a);
        }
        ('d', Span::Chars(a, b)) | ('c', Span::Chars(a, b)) => {
            if a != b {
                select(s, &a, &b);
                if s.run("edit.cut", &json!({})).is_err() {
                    caret(s, a);
                    return;
                }
                s.vim.lines = None;
            }
            caret(s, a);
            if op == 'c' {
                s.vim.mode = Mode::Insert;
            }
        }
        ('d', Span::Lines(f, l)) => {
            keep_lines(s, &f, &l);
            let (a, b) = lines_delete_range(s, &f, &l);
            select(s, &a, &b);
            if s.run("edit.cut", &json!({})).is_err() {
                caret(s, f);
                return;
            }
            note_clipboard(s);
            // The caret goes to the paragraph that took the deleted ones' place.
            let land = if a.off > 0 { sibling(s, &a, true).map(|path| Pos { story: a.story, path, off: 0 }).unwrap_or(at(&a, 0)) } else { a };
            caret(s, first_non_blank(s, &land));
        }
        ('c', Span::Lines(f, l)) => {
            let (a, b) = lines_chars(s, &f, &l);
            keep_lines(s, &f, &l);
            if a != b {
                select(s, &a, &b);
                if s.run("edit.cut", &json!({})).is_err() {
                    caret(s, a);
                    return;
                }
                note_clipboard(s);
            }
            caret(s, a);
            s.vim.mode = Mode::Insert;
        }
        _ => {}
    }
}

/// The remembered paragraphs, while the clipboard still holds what they left.
fn register_lines(s: &Session) -> Option<Vec<Block>> {
    let (blocks, text) = s.vim.lines.as_ref()?;
    let now = s.clipboard.as_ref().map(|f| f.clipboard_text()).unwrap_or_default();
    (now == *text && !blocks.is_empty()).then(|| blocks.clone())
}

/// Paste `frag` at the selection through Paste (one undo step, tracked like any paste), leaving
/// the clipboard as it was.
fn paste_fragment(s: &mut Session, frag: Fragment) -> bool {
    let kept = s.clipboard.replace(frag);
    let ok = s.run("edit.paste", &json!({})).is_ok();
    s.clipboard = kept;
    ok
}

/// `p` (after) or `P` (before), `n` times.
fn put(s: &mut Session, after: bool, n: usize) {
    let n = n.clamp(1, 100);
    let cur = s.sel.focus.clone();
    if let Some(blocks) = register_lines(s) {
        let many: Vec<Block> = (0..n).flat_map(|_| blocks.iter().cloned()).collect();
        // Pasted at a paragraph's end with an empty paragraph first, the paragraphs keep their
        // own formatting and the one before keeps its.
        let leading =
            |many: Vec<Block>| Fragment { blocks: std::iter::once(Block::Para(Paragraph::new())).chain(many).collect(), ..Default::default() };
        let host = if after { Some(at(&cur, 0)) } else { sibling(s, &cur, false).map(|path| Pos { story: cur.story, path, off: 0 }) };
        match host {
            Some(h) => {
                caret(s, end_of_para(s, &h));
                if paste_fragment(s, leading(many)) {
                    let first = sibling(s, &h, true).map_or(h, |path| Pos { story: cur.story, path, off: 0 });
                    caret(s, first_non_blank(s, &first));
                } else {
                    caret(s, cur);
                }
            }
            None => {
                // Above the container's first paragraph.
                caret(s, at(&cur, 0));
                let frag =
                    Fragment { blocks: many.into_iter().chain(std::iter::once(Block::Para(Paragraph::new()))).collect(), ..Default::default() };
                paste_fragment(s, frag);
                caret(s, first_non_blank(s, &at(&cur, 0)));
            }
        }
        return;
    }
    let Some(frag) = s.clipboard.clone() else { return };
    if after && cur.off < para_len(s, &cur) {
        caret(s, next_char(s, &cur));
    }
    let many = Fragment { blocks: (0..n).flat_map(|_| frag.blocks.iter().cloned()).collect(), parts: frag.parts.clone() };
    if paste_fragment(s, many) {
        let end = s.sel.focus.clone();
        caret(s, prev_char(s, &end));
    } else {
        caret(s, cur);
    }
}

/// `J`: join `n` paragraphs (at least two) with a space between them.
fn join(s: &mut Session, n: usize) {
    let cur = s.sel.focus.clone();
    let mut joined = false;
    for _ in 0..n.saturating_sub(1).max(1) {
        let Some(next) = sibling(s, &cur, true) else { break };
        let next = Pos { story: cur.story, path: next, off: 0 };
        let end = end_of_para(s, &cur);
        let text_before = s.doc.para_at(&cur).map(|x| x.text.clone()).unwrap_or_default();
        let from = first_non_blank(s, &next);
        let rest_empty = from.off >= para_len(s, &next);
        select(s, &end, &from);
        if joined {
            s.join_next_undo();
        }
        if s.run("text.delete", &json!({})).is_err() {
            break;
        }
        joined = true;
        let space = !text_before.is_empty() && !text_before.ends_with(char::is_whitespace) && !rest_empty;
        if space {
            caret(s, end.clone());
            s.join_next_undo();
            let _ = s.run("text.insert", &json!({"text": " ", "raw": true}));
        }
        caret(s, end);
    }
}

/// `r`: replace `n` characters with `c`.
fn replace_chars(s: &mut Session, c: char, n: usize) {
    let cur = s.sel.focus.clone();
    let end = chars_fwd(s, &cur, n);
    let have = s.doc.para_at(&cur).and_then(|x| x.text.get(cur.off..end.off)).map_or(0, |t| t.chars().count());
    if have < n || end == cur {
        return;
    }
    select(s, &cur, &end);
    if s.run("text.delete", &json!({})).is_err() {
        caret(s, cur);
        return;
    }
    s.join_next_undo();
    let text: String = std::iter::repeat_n(c, n).collect();
    let _ = s.run("text.insert", &json!({"text": text, "raw": true}));
    let end = s.sel.focus.clone();
    caret(s, prev_char(s, &end));
}

/// Change the case of a span (`~`, `u`, `U`), leaving the caret at `then`.
fn change_case(s: &mut Session, a: &Pos, b: &Pos, mode: &str, then: Pos) {
    if a != b {
        select(s, a, b);
        let _ = s.run("format.changeCase", &json!({"mode": mode}));
    }
    caret(s, then);
}

/// In Normal mode the caret sits before a character, never after a paragraph's last one.
fn settle(s: &mut Session) {
    if s.vim.mode != Mode::Normal || !s.sel.is_collapsed() {
        return;
    }
    let f = s.sel.focus.clone();
    let len = para_len(s, &f);
    if len > 0 && f.off >= len {
        let goal = s.goal_x;
        caret(s, prev_char(s, &at(&f, len)));
        s.goal_x = goal;
    }
}

// ---------------------------------------------------------------------------------------------
// Modes

fn insert_key(s: &mut Session, k: Key) -> bool {
    let run = |s: &mut Session, id: &str, v: Value| s.run(id, &v).is_ok();
    match k {
        Key::Esc => {
            s.vim.mode = Mode::Normal;
            if s.sel.is_collapsed() {
                let f = s.sel.focus.clone();
                caret(s, prev_char(s, &f));
            }
            settle(s);
            true
        }
        Key::Char('\t') => run(s, "text.tab", json!({})),
        Key::Char(c) => run(s, "text.insert", json!({"text": c.to_string()})),
        Key::Enter => run(s, "text.newParagraph", json!({})),
        Key::Backspace => run(s, "text.backspace", json!({})),
        Key::Delete => run(s, "text.delete", json!({})),
        Key::Left => run(s, "caret.left", json!({})),
        Key::Right => run(s, "caret.right", json!({})),
        Key::Up => run(s, "caret.up", json!({})),
        Key::Down => run(s, "caret.down", json!({})),
        Key::Ctrl(_) => false,
    }
}

fn normal_key(s: &mut Session, k: Key) -> bool {
    if k == Key::Esc {
        let had = !s.vim.pending.is_empty();
        s.vim.pending.clear();
        return had;
    }
    s.vim.pending.push(k);
    let keys = s.vim.pending.clone();
    match parse_normal(&keys) {
        Parse::More if keys.len() < MAX_PENDING => {}
        Parse::More | Parse::Bad => s.vim.pending.clear(),
        Parse::Done((count, cmd)) => {
            s.vim.pending.clear();
            normal(s, count, cmd);
            settle(s);
        }
    }
    true
}

fn normal(s: &mut Session, count: Option<usize>, cmd: Normal) {
    let n = count.unwrap_or(1).max(1);
    let cur = s.sel.focus.clone();
    match cmd {
        Normal::Move(m) => {
            if let Some((to, _)) = travel(s, &cur, m, count, false) {
                let goal = s.goal_x;
                caret(s, to);
                s.goal_x = goal;
            }
        }
        Normal::Op(op, target) => {
            let span = match target {
                Target::Lines => Some(Span::Lines(at(&cur, 0), paras_on(s, &cur, n - 1, true))),
                Target::Object(around, o) => object_span(s, &cur, around, o, n),
                // `cw` changes to the word's end, keeping the blanks after it.
                Target::Motion(Motion::WordFwd(big)) if op == 'c' && class_at(s, &cur, big) != 0 => {
                    travel(s, &cur, Motion::WordEnd(big), count, true).map(|(to, kind)| motion_span(s, &cur, &to, kind))
                }
                Target::Motion(m) => travel(s, &cur, m, count, true).map(|(to, kind)| motion_span(s, &cur, &to, kind)),
            };
            match span {
                Some(span) => operate(s, op, span),
                None => caret(s, cur),
            }
        }
        Normal::Replace(c) => replace_chars(s, c, n),
        Normal::Ctrl('r') => repeat_run(s, "edit.redo", n),
        Normal::Ctrl(c @ ('d' | 'u')) => {
            let to = display_move(s, &cur, if c == 'd' { "caret.down" } else { "caret.up" }, HALF_PAGE_LINES * n);
            caret(s, to);
        }
        Normal::Ctrl(c @ ('f' | 'b')) => {
            caret(s, cur);
            repeat_run(s, if c == 'f' { "caret.pageDown" } else { "caret.pageUp" }, n);
        }
        Normal::Ctrl(_) => {}
        Normal::Simple(c) => simple(s, c, count, n, cur),
    }
}

fn repeat_run(s: &mut Session, id: &str, n: usize) {
    for _ in 0..n.min(MAX_COUNT) {
        if s.run(id, &json!({})).is_err() {
            break;
        }
    }
    // Undo and Redo bring back the selection an edit was made with: in Normal mode the caret
    // goes to its start, as Vim's cursor goes to the change.
    if !s.sel.is_collapsed() {
        let (a, _) = s.sel.ordered();
        caret(s, a);
    }
}

fn simple(s: &mut Session, c: char, count: Option<usize>, n: usize, cur: Pos) {
    let line_end = |s: &Session| end_of_para(s, &paras_on(s, &cur, n - 1, true));
    match c {
        'x' => operate(s, 'd', Span::Chars(cur.clone(), chars_fwd(s, &cur, n))),
        'X' => operate(s, 'd', Span::Chars(chars_back(s, &cur, n), cur.clone())),
        'D' => {
            let e = line_end(s);
            operate(s, 'd', Span::Chars(cur, e));
        }
        'C' => {
            let e = line_end(s);
            operate(s, 'c', Span::Chars(cur, e));
        }
        's' => operate(s, 'c', Span::Chars(cur.clone(), chars_fwd(s, &cur, n))),
        'S' => operate(s, 'c', Span::Lines(at(&cur, 0), paras_on(s, &cur, n - 1, true))),
        'Y' => operate(s, 'y', Span::Lines(at(&cur, 0), paras_on(s, &cur, n - 1, true))),
        'p' | 'P' => put(s, c == 'p', n),
        'J' => join(s, n),
        '~' => {
            let e = chars_fwd(s, &cur, n);
            let then = if e.off < para_len(s, &e) { e.clone() } else { cur.clone() };
            change_case(s, &cur, &e, "toggle", then);
        }
        'u' => repeat_run(s, "edit.undo", n),
        'i' => s.vim.mode = Mode::Insert,
        'a' => {
            caret(s, next_char(s, &cur));
            s.vim.mode = Mode::Insert;
        }
        'I' => {
            caret(s, first_non_blank(s, &cur));
            s.vim.mode = Mode::Insert;
        }
        'A' => {
            caret(s, end_of_para(s, &cur));
            s.vim.mode = Mode::Insert;
        }
        'o' => {
            caret(s, end_of_para(s, &cur));
            if s.run("text.newParagraph", &json!({"raw": true})).is_ok() {
                s.vim.mode = Mode::Insert;
            }
        }
        'O' => {
            caret(s, at(&cur, 0));
            if s.run("text.newParagraph", &json!({"raw": true})).is_ok() {
                // The new empty paragraph is the one above.
                let f = s.sel.focus.clone();
                if let Some(path) = sibling(s, &f, false) {
                    caret(s, Pos { story: f.story, path, off: 0 });
                }
                s.vim.mode = Mode::Insert;
            }
        }
        'v' | 'V' => start_visual(s, if c == 'v' { Mode::Visual } else { Mode::VisualLine }, cur.clone(), cur),
        'n' | 'N' => find_again(s, c == 'n', n),
        '*' | '#' => find_word(s, &cur, c == '*'),
        '/' | '?' => {
            let _ = s.run("edit.find", &json!({}));
        }
        _ => {
            let _ = count;
        }
    }
}

/// `n` / `N`: the next or previous match of the last search, the caret at its start.
fn find_again(s: &mut Session, fwd: bool, n: usize) {
    let cur = s.sel.focus.clone();
    for _ in 0..n.min(MAX_COUNT) {
        if s.run(if fwd { "edit.findNext" } else { "edit.findPrevious" }, &json!({})).is_err() {
            caret(s, cur);
            return;
        }
    }
    let (a, _) = s.sel.ordered();
    caret(s, a);
}

/// `*` / `#`: search for the word at the caret, forward or back.
fn find_word(s: &mut Session, cur: &Pos, fwd: bool) {
    let Some(para) = s.doc.para_at(cur) else { return };
    let (a, b) = para.word_at(cur.off);
    let Some(word) = para.text.get(a..b).map(|w| w.trim().to_string()).filter(|w| !w.is_empty()) else { return };
    caret(s, at(cur, if fwd { b } else { a }));
    if s.run("edit.find", &json!({"text": word, "wholeWord": true, "matchCase": true})).is_err() {
        caret(s, cur.clone());
        return;
    }
    if !fwd {
        let _ = s.run("edit.findPrevious", &json!({}));
    }
    let (a, _) = s.sel.ordered();
    caret(s, a);
}

fn start_visual(s: &mut Session, mode: Mode, anchor: Pos, cursor: Pos) {
    s.vim.mode = mode;
    s.vim.visual = Some(Visual { anchor, cursor, shown: s.sel.clone() });
    show_visual(s);
}

/// Select what Visual mode covers: both ends' characters, or their whole paragraphs.
fn show_visual(s: &mut Session) {
    let Some(v) = s.vim.visual.clone() else { return };
    let fwd = v.cursor >= v.anchor;
    s.sel = match s.vim.mode {
        Mode::VisualLine => {
            let (first, last) = if fwd { (&v.anchor, &v.cursor) } else { (&v.cursor, &v.anchor) };
            let (a, b) = lines_chars(s, first, last);
            if fwd { Selection { anchor: a, focus: b } } else { Selection { anchor: b, focus: a } }
        }
        _ if fwd => Selection { anchor: v.anchor.clone(), focus: next_char(s, &v.cursor) },
        _ => Selection { anchor: next_char(s, &v.anchor), focus: v.cursor.clone() },
    };
    let shown = s.sel.clone();
    if let Some(v) = s.vim.visual.as_mut() {
        v.shown = shown;
    }
}

fn leave_visual(s: &mut Session, to: Pos) {
    s.vim.mode = Mode::Normal;
    s.vim.visual = None;
    caret(s, to);
    settle(s);
}

/// The span Visual mode covers.
fn visual_span(s: &Session) -> Option<Span> {
    let v = s.vim.visual.as_ref()?;
    let (lo, hi) = if v.cursor >= v.anchor { (&v.anchor, &v.cursor) } else { (&v.cursor, &v.anchor) };
    Some(match s.vim.mode {
        Mode::VisualLine => Span::Lines(at(lo, 0), at(hi, 0)),
        _ => Span::Chars(lo.clone(), next_char(s, hi)),
    })
}

fn visual_key(s: &mut Session, k: Key) -> bool {
    let Some(v) = s.vim.visual.clone() else {
        s.vim.mode = Mode::Normal;
        return normal_key(s, k);
    };
    if k == Key::Esc {
        s.vim.pending.clear();
        leave_visual(s, v.cursor);
        return true;
    }
    s.vim.pending.push(k);
    let keys = s.vim.pending.clone();
    let mut r = Reader { keys: &keys, i: 0 };
    let count = r.count();
    let n = count.unwrap_or(1).max(1);
    let Some(key) = r.peek() else { return true };
    let span = visual_span(s);
    let lo = match &span {
        Some(Span::Chars(a, _)) | Some(Span::Lines(a, _)) => a.clone(),
        None => v.cursor.clone(),
    };
    let done = |s: &mut Session| s.vim.pending.clear();
    match key {
        Key::Char(c @ ('d' | 'x' | 'X' | 'D' | 'y' | 'Y' | 'c' | 's' | 'C' | 'S' | 'R' | '>' | '<')) => {
            done(s);
            let lines = s.vim.mode == Mode::VisualLine || matches!(c, 'X' | 'D' | 'Y' | 'C' | 'S' | 'R');
            let span = match span {
                Some(Span::Chars(a, b)) if lines => Some(Span::Lines(at(&a, 0), at(&prev_char(s, &b), 0))),
                other => other,
            };
            s.vim.mode = Mode::Normal;
            s.vim.visual = None;
            let op = match c {
                'd' | 'x' | 'X' | 'D' => 'd',
                'y' | 'Y' => 'y',
                '>' | '<' => c,
                _ => 'c',
            };
            if let Some(span) = span {
                operate(s, op, span);
            }
            settle(s);
        }
        Key::Char(c @ ('~' | 'u' | 'U')) => {
            done(s);
            if let Some(span) = span {
                let (a, b) = match &span {
                    Span::Chars(a, b) => (a.clone(), b.clone()),
                    Span::Lines(f, l) => lines_chars(s, f, l),
                };
                s.vim.mode = Mode::Normal;
                s.vim.visual = None;
                change_case(
                    s,
                    &a,
                    &b,
                    if c == '~' {
                        "toggle"
                    } else if c == 'u' {
                        "lower"
                    } else {
                        "upper"
                    },
                    lo,
                );
                settle(s);
            }
        }
        Key::Char('J') => {
            done(s);
            let paras = match &span {
                Some(Span::Lines(f, l)) | Some(Span::Chars(f, l)) => s.doc.para_paths_in(f, l).len(),
                None => 1,
            };
            leave_visual(s, at(&lo, 0));
            join(s, paras.max(2));
            settle(s);
        }
        Key::Char(c @ ('p' | 'P')) => {
            done(s);
            // The selection is replaced by what was yanked.
            let (a, b) = match &span {
                Some(Span::Chars(a, b)) => (a.clone(), b.clone()),
                Some(Span::Lines(f, l)) => lines_chars(s, f, l),
                None => (lo.clone(), lo.clone()),
            };
            s.vim.mode = Mode::Normal;
            s.vim.visual = None;
            if let Some(frag) = s.clipboard.clone() {
                select(s, &a, &b);
                let _ = c;
                if paste_fragment(s, frag) {
                    let end = s.sel.focus.clone();
                    caret(s, prev_char(s, &end));
                } else {
                    caret(s, a);
                }
            } else {
                caret(s, a);
            }
            settle(s);
        }
        Key::Char('o') => {
            done(s);
            if let Some(v) = s.vim.visual.as_mut() {
                std::mem::swap(&mut v.anchor, &mut v.cursor);
            }
            show_visual(s);
        }
        Key::Char(c @ ('v' | 'V')) => {
            done(s);
            let mode = if c == 'v' { Mode::Visual } else { Mode::VisualLine };
            if s.vim.mode == mode {
                leave_visual(s, v.cursor);
            } else {
                s.vim.mode = mode;
                show_visual(s);
            }
        }
        Key::Char(around @ ('i' | 'a')) => match r.keys.get(r.i + 1) {
            None => {}
            Some(Key::Char(o @ ('w' | 'W' | 's' | 'p'))) => {
                done(s);
                match object_span(s, &v.cursor, around == 'a', *o, n) {
                    Some(Span::Chars(a, b)) if a != b => {
                        let b = prev_char(s, &b);
                        if let Some(v) = s.vim.visual.as_mut() {
                            v.anchor = a;
                            v.cursor = b;
                        }
                    }
                    Some(Span::Lines(a, b)) => {
                        s.vim.mode = Mode::VisualLine;
                        if let Some(v) = s.vim.visual.as_mut() {
                            v.anchor = a;
                            v.cursor = b;
                        }
                    }
                    _ => {}
                }
                show_visual(s);
            }
            Some(_) => done(s),
        },
        _ => match motion(&mut r) {
            Parse::More if keys.len() < MAX_PENDING => {}
            Parse::More | Parse::Bad => done(s),
            Parse::Done(m) => {
                done(s);
                if let Some((to, _)) = travel(s, &v.cursor, m, count, false) {
                    // A Visual cursor sits on a character, not after a paragraph's last one.
                    let len = para_len(s, &to);
                    let to = if len > 0 && to.off >= len { prev_char(s, &at(&to, len)) } else { to };
                    if let Some(v) = s.vim.visual.as_mut() {
                        v.cursor = to;
                    }
                }
                show_visual(s);
            }
        },
    }
    true
}

/// Where Normal mode's block cursor ends: the caret stop after the caret, in its paragraph, or
/// `None` at a paragraph's end (an empty one).
pub fn block_end(s: &Session) -> Option<Pos> {
    if !s.prefs.vim_keys || s.vim.mode != Mode::Normal || !s.sel.is_collapsed() {
        return None;
    }
    let f = &s.sel.focus;
    let e = next_char(s, f);
    (e != *f).then_some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(paras: &[&str]) -> Session {
        let text = paras.join("\n");
        let d = crate::io::open_bytes("v.txt", text.as_bytes()).unwrap();
        let mut s = Session::new(d);
        s.run("vim.toggle", &json!({"value": true})).unwrap();
        s
    }

    fn keys(s: &mut Session, k: &str) -> Value {
        s.run("vim.keys", &json!({"keys": k})).unwrap()
    }

    fn texts(s: &Session) -> Vec<String> {
        s.doc.body.iter().filter_map(|b| b.as_para()).map(|p| p.text.clone()).collect()
    }

    fn caret_at(s: &Session) -> (usize, usize) {
        (s.sel.focus.path.last(), s.sel.focus.off)
    }

    #[test]
    fn parses_vim_notation() {
        assert_eq!(
            parse_keys("d<Esc>x<C-r><lt><CR><b>"),
            vec![
                Key::Char('d'),
                Key::Esc,
                Key::Char('x'),
                Key::Ctrl('r'),
                Key::Char('<'),
                Key::Enter,
                Key::Char('<'),
                Key::Char('b'),
                Key::Char('>'),
            ]
        );
    }

    #[test]
    fn off_by_default_and_toggles() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        assert!(s.run("vim.keys", &json!({"keys": "x"})).is_err());
        let r = s.run("vim.toggle", &json!({})).unwrap();
        assert_eq!(r["value"], true);
        assert_eq!(r["mode"], "normal");
        assert!(s.prefs.vim_keys);
    }

    #[test]
    fn word_motions() {
        let mut s = doc(&["one two, three", "", "four"]);
        keys(&mut s, "w");
        assert_eq!(caret_at(&s), (0, 4));
        keys(&mut s, "w");
        assert_eq!(caret_at(&s), (0, 7), "punctuation is a word of its own");
        keys(&mut s, "w");
        assert_eq!(caret_at(&s), (0, 9));
        keys(&mut s, "w");
        assert_eq!(caret_at(&s), (1, 0), "an empty paragraph stops w");
        keys(&mut s, "w");
        assert_eq!(caret_at(&s), (2, 0));
        keys(&mut s, "b");
        assert_eq!(caret_at(&s), (1, 0));
        keys(&mut s, "gg");
        keys(&mut s, "e");
        assert_eq!(caret_at(&s), (0, 2));
        keys(&mut s, "2e");
        assert_eq!(caret_at(&s), (0, 7));
        keys(&mut s, "0W");
        assert_eq!(caret_at(&s), (0, 4));
        keys(&mut s, "W");
        assert_eq!(caret_at(&s), (0, 9), "WORDs include punctuation");
    }

    #[test]
    fn line_motions_and_counts() {
        let mut s = doc(&["  indented line", "second"]);
        keys(&mut s, "$");
        assert_eq!(caret_at(&s), (0, 14), "$ rests on the last character");
        keys(&mut s, "^");
        assert_eq!(caret_at(&s), (0, 2));
        keys(&mut s, "0");
        assert_eq!(caret_at(&s), (0, 0));
        keys(&mut s, "3l");
        assert_eq!(caret_at(&s), (0, 3));
        keys(&mut s, "G");
        assert_eq!(caret_at(&s), (1, 0));
        keys(&mut s, "fc");
        assert_eq!(caret_at(&s), (1, 2));
        keys(&mut s, "0;");
        assert_eq!(caret_at(&s), (1, 2));
        keys(&mut s, "$Fe");
        assert_eq!(caret_at(&s), (1, 1));
        keys(&mut s, "1G");
        assert_eq!(caret_at(&s), (0, 2), "a count picks the paragraph");
    }

    #[test]
    fn delete_change_and_yank_words() {
        let mut s = doc(&["alpha beta gamma"]);
        keys(&mut s, "dw");
        assert_eq!(texts(&s), ["beta gamma"]);
        keys(&mut s, "cwBETA<Esc>");
        assert_eq!(texts(&s), ["BETA gamma"]);
        assert_eq!(s.vim.mode, Mode::Normal);
        assert_eq!(caret_at(&s), (0, 3), "Escape steps back onto the last typed character");
        keys(&mut s, "wyiw0P");
        assert_eq!(texts(&s), ["gammaBETA gamma"]);
        keys(&mut s, "u");
        assert_eq!(texts(&s), ["BETA gamma"]);
        keys(&mut s, "$daw");
        assert_eq!(texts(&s), ["BETA"]);
        keys(&mut s, "x");
        assert_eq!(texts(&s), ["BET"], "x at the end takes the last character");
        keys(&mut s, "0d$");
        assert_eq!(texts(&s), [""]);
    }

    #[test]
    fn dw_on_a_last_word_keeps_the_paragraph_mark() {
        let mut s = doc(&["one two", "three"]);
        keys(&mut s, "wdw");
        assert_eq!(texts(&s), ["one ", "three"]);
    }

    #[test]
    fn paragraphs_are_lines() {
        let mut s = doc(&["one", "two", "three", "four"]);
        keys(&mut s, "jdd");
        assert_eq!(texts(&s), ["one", "three", "four"]);
        assert_eq!(caret_at(&s), (1, 0));
        keys(&mut s, "p");
        assert_eq!(texts(&s), ["one", "three", "two", "four"]);
        assert_eq!(caret_at(&s), (2, 0), "the caret goes to the put paragraph");
        keys(&mut s, "ggP");
        assert_eq!(texts(&s), ["two", "one", "three", "two", "four"]);
        keys(&mut s, "G2yyP");
        assert_eq!(texts(&s), ["two", "one", "three", "two", "four", "four"], "2yy on the last paragraph takes just it");
        keys(&mut s, "gg2dd");
        assert_eq!(texts(&s), ["three", "two", "four", "four"]);
        keys(&mut s, "Gdd");
        assert_eq!(texts(&s), ["three", "two", "four"], "dd on the last paragraph takes the mark before it");
        keys(&mut s, "ggdj");
        assert_eq!(texts(&s), ["four"]);
        keys(&mut s, "dd");
        assert_eq!(texts(&s), [""]);
    }

    #[test]
    fn put_below_keeps_the_paragraphs_formatting() {
        let mut s = doc(&["Title", "Body"]);
        s.run("para.style", &json!({"style": "Heading1"})).unwrap();
        keys(&mut s, "yyjp");
        let styles: Vec<Option<String>> = s.doc.body.iter().filter_map(|b| b.as_para()).map(|p| p.props.style.clone()).collect();
        assert_eq!(texts(&s), ["Title", "Body", "Title"]);
        assert_eq!(styles[1], None, "the paragraph above keeps its own");
        assert_eq!(styles[2].as_deref(), Some("Heading1"));
    }

    #[test]
    fn insert_commands() {
        let mut s = doc(&["middle"]);
        keys(&mut s, "Otop<Esc>");
        keys(&mut s, "jobottom<Esc>");
        assert_eq!(texts(&s), ["top", "middle", "bottom"]);
        keys(&mut s, "kI> <Esc>A!<Esc>");
        assert_eq!(texts(&s), ["top", "> middle!", "bottom"]);
        keys(&mut s, "0lax<Esc>");
        assert_eq!(texts(&s), ["top", "> xmiddle!", "bottom"]);
        keys(&mut s, "ccnew<Esc>");
        assert_eq!(texts(&s), ["top", "new", "bottom"]);
    }

    #[test]
    fn replace_join_and_case() {
        let mut s = doc(&["abc", "  def"]);
        keys(&mut s, "2rx");
        assert_eq!(texts(&s), ["xxc", "  def"]);
        keys(&mut s, "u");
        assert_eq!(texts(&s)[0], "abc", "r is one undo step");
        keys(&mut s, "J");
        assert_eq!(texts(&s), ["abc def"]);
        keys(&mut s, "u");
        assert_eq!(texts(&s), ["abc", "  def"], "J is one undo step");
        keys(&mut s, "gg~~");
        assert_eq!(texts(&s)[0], "ABc");
        keys(&mut s, "0vlu");
        assert_eq!(texts(&s)[0], "abc");
    }

    #[test]
    fn visual_modes() {
        let mut s = doc(&["one two three", "four"]);
        keys(&mut s, "wve");
        assert_eq!(s.selected_text(), "two");
        assert_eq!(s.vim.mode, Mode::Visual);
        keys(&mut s, "d");
        assert_eq!(texts(&s), ["one  three", "four"]);
        assert_eq!(s.vim.mode, Mode::Normal);
        keys(&mut s, "Vjy");
        assert!(s.sel.is_collapsed());
        keys(&mut s, "Gp");
        assert_eq!(texts(&s), ["one  three", "four", "one  three", "four"]);
        keys(&mut s, "ggviwc1<Esc>");
        assert_eq!(texts(&s)[0], "1  three");
        keys(&mut s, "v<Esc>");
        assert_eq!(s.vim.mode, Mode::Normal);
        assert!(s.sel.is_collapsed());
    }

    #[test]
    fn a_mouse_selection_is_visual_mode() {
        let mut s = doc(&["hello world"]);
        s.sel = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 5) };
        keys(&mut s, "d");
        assert_eq!(texts(&s), [" world"]);
    }

    #[test]
    fn escape_with_nothing_to_cancel_is_left_to_the_window() {
        let mut s = doc(&["x"]);
        assert_eq!(keys(&mut s, "<Esc>")["handled"], false);
        assert_eq!(keys(&mut s, "d")["pending"], "d");
        assert_eq!(keys(&mut s, "<Esc>")["handled"], true);
        assert_eq!(keys(&mut s, "")["pending"], "");
    }

    #[test]
    fn counts_and_junk_stay_bounded() {
        let mut s = doc(&["a b c"]);
        keys(&mut s, "99999999999999999999x");
        assert_eq!(texts(&s), [""]);
        keys(&mut s, "zqZQ<C-x>gZ\u{1F600}");
        assert_eq!(s.vim.pending(), "");
        let long = "d".repeat(MAX_KEYS + 1);
        assert!(s.run("vim.keys", &json!({"keys": long})).is_err());
    }
}
