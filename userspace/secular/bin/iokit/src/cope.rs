// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inline vi-style editor drawn below the shell prompt.

#[cfg(target_os = "fuchsia")]
use fidl_fuchsia_hardware_pty as fpty;
use std::io::{self, ErrorKind, IsTerminal, Write};
use std::mem::MaybeUninit;
use std::process::ExitCode;

const USAGE: &str = "Usage: cope [-R] [file]";
const HELP: &str = "\
Usage: cope [-R] [file]

Inline vi-style editor. The buffer is drawn below the prompt and does not
switch to an alternate screen.

Normal:  h j k l  0 ^ $  w b e  gg G  x X  dd cc yy  p P  u  J  ~  r R
         i a I A  o O  :  ZZ  ZQ
Insert:  type text; ESC / Ctrl-[ / Ctrl-C returns to normal
Replace: R overwrites until ESC; r{char} replaces one character
Ex:      :w  :w file  :q  :q!  :wq  :x  :{line}  :$
";

const DEFAULT_COLUMNS: usize = 80;
const DEFAULT_TERMINAL_ROWS: usize = 24;
const DEFAULT_VIEW_ROWS: usize = 16;
const MIN_VIEW_ROWS: usize = 8;
const MAX_UNDO: usize = 64;
const ESCAPE_TIMEOUT_MS: i32 = 50;
const BELL: &[u8] = b"\x07";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const ERASE_LINE: &[u8] = b"\r\x1b[2K";
const QUERY_CURSOR: &[u8] = b"\x1b[6n";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Options {
    Help,
    Edit { path: Option<String>, readonly: bool },
}

/// Parses operands after `argv[0]`. `-R` opens the buffer read-only.
fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options_ended = false;
    let mut help = false;
    let mut readonly = false;
    let mut path = None;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && (arg == "--help" || arg == "-h") {
            help = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            for flag in arg.chars().skip(1) {
                match flag {
                    'h' => help = true,
                    'R' => readonly = true,
                    _ => return Err(format!("cope: invalid option -- '{flag}'\n{USAGE}")),
                }
            }
            continue;
        }
        if path.is_some() {
            return Err(format!("cope: extra operand '{arg}'\n{USAGE}"));
        }
        path = Some(arg.clone());
    }
    if help {
        return Ok(Options::Help);
    }
    Ok(Options::Edit { path, readonly })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Char(char),
    Escape,
    Enter,
    Backspace,
    Delete,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Ctrl(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
    Replace,
    ReplaceChar,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Delete,
    Change,
    Yank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    WordForward,
    WordBack,
    WordEnd,
    FileStart,
    FileEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MotionKind {
    Charwise { inclusive: bool },
    Linewise,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cursor {
    row: usize,
    column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Register {
    Empty,
    Chars(String),
    Lines(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Cursor,
    dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopControl {
    Continue,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExCommand {
    Write { path: Option<String>, force: bool, quit: bool },
    Quit { force: bool },
    GoToLine(usize),
    GoToLast,
}

#[derive(Clone, Debug)]
struct Editor {
    lines: Vec<String>,
    cursor: Cursor,
    preferred_column: usize,
    mode: Mode,
    count: usize,
    operator: Option<Operator>,
    operator_count: usize,
    pending_g: bool,
    pending_z: bool,
    register: Register,
    undo: Vec<Snapshot>,
    path: Option<String>,
    readonly: bool,
    dirty: bool,
    is_new_file: bool,
    message: String,
    command: String,
    view_origin: usize,
    view_rows: usize,
    view_columns: usize,
    beep: bool,
}

impl Editor {
    fn from_text(path: Option<String>, text: &str, readonly: bool, is_new_file: bool) -> Self {
        let lines = lines_from_text(text);
        let mut editor = Self {
            lines,
            cursor: Cursor::default(),
            preferred_column: 0,
            mode: Mode::Normal,
            count: 0,
            operator: None,
            operator_count: 0,
            pending_g: false,
            pending_z: false,
            register: Register::Empty,
            undo: Vec::new(),
            path,
            readonly,
            dirty: false,
            is_new_file,
            message: String::new(),
            command: String::new(),
            view_origin: 0,
            view_rows: DEFAULT_VIEW_ROWS,
            view_columns: DEFAULT_COLUMNS,
            beep: false,
        };
        editor.set_open_message(text.len());
        editor
    }

    fn set_view_size(&mut self, columns: usize, rows: usize) {
        self.view_columns = columns.max(1);
        self.view_rows = rows.max(MIN_VIEW_ROWS);
        self.sync_view();
    }

    fn set_open_message(&mut self, byte_count: usize) {
        self.message = match &self.path {
            Some(path) if self.is_new_file => format!("\"{path}\" [New File]"),
            Some(path) => format!("\"{path}\" {}L, {byte_count}C", self.lines.len()),
            None => format!("[No Name] {}L, {byte_count}C", self.lines.len()),
        };
        if self.readonly {
            self.message.push_str(" [readonly]");
        }
    }

    fn to_text(&self) -> String {
        text_from_lines(&self.lines)
    }

    fn content_rows(&self) -> usize {
        self.view_rows.saturating_sub(1).max(1)
    }

    fn in_insert_like(&self) -> bool {
        matches!(self.mode, Mode::Insert | Mode::Replace)
    }

    fn line_char_count(&self, row: usize) -> usize {
        self.lines.get(row).map(|line| line.chars().count()).unwrap_or(0)
    }

    fn first_non_blank_at(&self, row: usize) -> usize {
        match self.lines.get(row) {
            Some(line) => line.chars().position(|ch| !ch.is_whitespace()).unwrap_or(line.chars().count()),
            None => 0,
        }
    }

    fn clamp_column(&self, row: usize, preferred: usize) -> usize {
        let length = self.line_char_count(row);
        if self.in_insert_like() {
            preferred.min(length)
        } else if length == 0 {
            0
        } else {
            preferred.min(length - 1)
        }
    }

    fn ensure_invariants(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        if self.cursor.row >= self.lines.len() {
            self.cursor.row = self.lines.len() - 1;
        }
        self.cursor.column = self.clamp_column(self.cursor.row, self.cursor.column);
    }

    fn sync_view(&mut self) {
        self.ensure_invariants();
        let content_rows = self.content_rows();
        if self.cursor.row < self.view_origin {
            self.view_origin = self.cursor.row;
        } else if self.cursor.row >= self.view_origin.saturating_add(content_rows) {
            self.view_origin = self.cursor.row + 1 - content_rows;
        }
        let max_origin = self.lines.len().saturating_sub(content_rows);
        if self.view_origin > max_origin {
            self.view_origin = max_origin;
        }
    }

    fn push_undo(&mut self) {
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor,
            dirty: self.dirty,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
    }

    fn reset_pending(&mut self) {
        self.count = 0;
        self.operator = None;
        self.operator_count = 0;
        self.pending_g = false;
        self.pending_z = false;
    }

    fn take_count(&mut self) -> usize {
        let count = if self.count == 0 { 1 } else { self.count };
        self.count = 0;
        count
    }

    fn operator_repeats(&mut self) -> usize {
        let prefix = if self.count == 0 { 1 } else { self.count };
        let inner = if self.operator_count == 0 { 1 } else { self.operator_count };
        self.count = 0;
        self.operator_count = 0;
        prefix.saturating_mul(inner)
    }

    fn handle_key(&mut self, key: Key) -> LoopControl {
        self.beep = false;
        if self.mode != Mode::Command {
            self.message.clear();
        }
        self.ensure_invariants();
        let control = match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Replace => self.handle_replace(key),
            Mode::ReplaceChar => self.handle_replace_char(key),
            Mode::Command => self.handle_command(key),
        };
        self.ensure_invariants();
        self.sync_view();
        control
    }

    fn handle_normal(&mut self, key: Key) -> LoopControl {
        if self.pending_g {
            self.pending_g = false;
            return self.handle_after_g(key);
        }
        if self.pending_z {
            self.pending_z = false;
            return self.handle_z_command(key);
        }
        if self.operator.is_some() {
            return self.handle_operator_key(key);
        }
        match key {
            Key::Char(digit) if digit.is_ascii_digit() && (digit != '0' || self.count > 0) => {
                let value = (digit as u8 - b'0') as usize;
                self.count = self.count.saturating_mul(10).saturating_add(value);
                LoopControl::Continue
            }
            Key::Escape | Key::Ctrl('c') | Key::Ctrl('[') => {
                self.reset_pending();
                LoopControl::Continue
            }
            Key::Char('i') => self.enter_insert(false),
            Key::Char('a') => self.enter_insert(true),
            Key::Char('I') => {
                self.cursor.column = self.first_non_blank_at(self.cursor.row);
                self.preferred_column = self.cursor.column;
                self.enter_insert(false)
            }
            Key::Char('A') => {
                self.cursor.column = self.line_char_count(self.cursor.row);
                self.preferred_column = self.cursor.column;
                self.enter_insert(false)
            }
            Key::Char('o') => self.open_line(false),
            Key::Char('O') => self.open_line(true),
            Key::Char('R') => {
                self.push_undo();
                self.mode = Mode::Replace;
                self.reset_pending();
                LoopControl::Continue
            }
            Key::Char('r') => {
                self.mode = Mode::ReplaceChar;
                LoopControl::Continue
            }
            Key::Char('x') | Key::Delete => {
                let count = self.take_count();
                self.delete_chars(count, false);
                LoopControl::Continue
            }
            Key::Char('X') => {
                let count = self.take_count();
                self.delete_chars(count, true);
                LoopControl::Continue
            }
            Key::Char('d') => {
                self.operator = Some(Operator::Delete);
                LoopControl::Continue
            }
            Key::Char('c') => {
                self.operator = Some(Operator::Change);
                LoopControl::Continue
            }
            Key::Char('y') => {
                self.operator = Some(Operator::Yank);
                LoopControl::Continue
            }
            Key::Char('D') => {
                self.apply_operator_motion(Operator::Delete, Motion::LineEnd, 1);
                LoopControl::Continue
            }
            Key::Char('C') => {
                self.apply_operator_motion(Operator::Change, Motion::LineEnd, 1);
                LoopControl::Continue
            }
            Key::Char('Y') => {
                let count = self.take_count();
                self.apply_line_operator(Operator::Yank, count);
                LoopControl::Continue
            }
            Key::Char('p') => {
                self.put(false);
                LoopControl::Continue
            }
            Key::Char('P') => {
                self.put(true);
                LoopControl::Continue
            }
            Key::Char('u') => {
                self.undo_last();
                LoopControl::Continue
            }
            Key::Char('J') => {
                let count = self.take_count();
                self.join_lines(count);
                LoopControl::Continue
            }
            Key::Char('~') => {
                let count = self.take_count();
                self.toggle_case(count);
                LoopControl::Continue
            }
            Key::Char('g') => {
                self.pending_g = true;
                LoopControl::Continue
            }
            Key::Char('G') => {
                self.goto_line(true);
                LoopControl::Continue
            }
            Key::Char(':') => {
                self.reset_pending();
                self.mode = Mode::Command;
                self.command.clear();
                LoopControl::Continue
            }
            Key::Char('Z') => {
                self.pending_z = true;
                LoopControl::Continue
            }
            Key::Ctrl('l') => LoopControl::Continue,
            Key::Ctrl('d') => LoopControl::Continue,
            other => {
                if let Some(motion) = motion_from_key(other) {
                    let repeats = self.take_count();
                    self.apply_motion(motion, repeats);
                    LoopControl::Continue
                } else {
                    self.reset_pending();
                    self.beep = true;
                    LoopControl::Continue
                }
            }
        }
    }

    fn handle_z_command(&mut self, key: Key) -> LoopControl {
        match key {
            Key::Char('Z') => self.write_and_maybe_quit(true),
            Key::Char('Q') => {
                if self.dirty {
                    self.dirty = false;
                }
                LoopControl::Quit
            }
            _ => {
                self.beep = true;
                LoopControl::Continue
            }
        }
    }

    fn handle_after_g(&mut self, key: Key) -> LoopControl {
        match key {
            Key::Char('g') => {
                if let Some(operator) = self.operator.take() {
                    self.apply_operator_motion(operator, Motion::FileStart, 1);
                } else {
                    self.goto_line(false);
                }
                LoopControl::Continue
            }
            Key::Escape | Key::Ctrl('c') => {
                self.reset_pending();
                LoopControl::Continue
            }
            _ => {
                self.reset_pending();
                self.beep = true;
                LoopControl::Continue
            }
        }
    }

    fn handle_operator_key(&mut self, key: Key) -> LoopControl {
        if let Key::Char(digit) = key
            && digit.is_ascii_digit()
            && (digit != '0' || self.operator_count > 0)
        {
            let value = (digit as u8 - b'0') as usize;
            self.operator_count = self.operator_count.saturating_mul(10).saturating_add(value);
            return LoopControl::Continue;
        }
        if matches!(key, Key::Escape | Key::Ctrl('c') | Key::Ctrl('[')) {
            self.reset_pending();
            return LoopControl::Continue;
        }
        if key == Key::Char('g') {
            self.pending_g = true;
            return LoopControl::Continue;
        }
        let operator = match self.operator {
            Some(operator) => operator,
            None => return LoopControl::Continue,
        };
        let same_line = matches!(
            (operator, key),
            (Operator::Delete, Key::Char('d'))
                | (Operator::Change, Key::Char('c'))
                | (Operator::Yank, Key::Char('y'))
        );
        if same_line {
            let repeats = self.operator_repeats();
            self.apply_line_operator(operator, repeats);
            return LoopControl::Continue;
        }
        if let Some(motion) = motion_from_key(key) {
            let repeats = self.operator_repeats();
            self.apply_operator_motion(operator, motion, repeats);
            LoopControl::Continue
        } else {
            self.reset_pending();
            self.beep = true;
            LoopControl::Continue
        }
    }

    fn handle_insert(&mut self, key: Key) -> LoopControl {
        match key {
            Key::Escape | Key::Ctrl('c') | Key::Ctrl('[') => {
                self.leave_insert();
                LoopControl::Continue
            }
            Key::Enter => {
                self.split_line();
                LoopControl::Continue
            }
            Key::Backspace | Key::Ctrl('h') => {
                self.backspace();
                LoopControl::Continue
            }
            Key::Delete => {
                self.delete_chars(1, false);
                LoopControl::Continue
            }
            Key::Ctrl('w') => {
                self.backspace_word();
                LoopControl::Continue
            }
            Key::Ctrl('u') => {
                self.backspace_to_line_start();
                LoopControl::Continue
            }
            Key::Tab => {
                self.insert_char('\t');
                LoopControl::Continue
            }
            Key::Char(character) => {
                self.insert_char(character);
                LoopControl::Continue
            }
            Key::Left => {
                self.apply_motion(Motion::Left, 1);
                LoopControl::Continue
            }
            Key::Right => {
                self.apply_motion(Motion::Right, 1);
                LoopControl::Continue
            }
            Key::Up => {
                self.apply_motion(Motion::Up, 1);
                LoopControl::Continue
            }
            Key::Down => {
                self.apply_motion(Motion::Down, 1);
                LoopControl::Continue
            }
            Key::Home => {
                self.apply_motion(Motion::LineStart, 1);
                LoopControl::Continue
            }
            Key::End => {
                self.cursor.column = self.line_char_count(self.cursor.row);
                self.preferred_column = self.cursor.column;
                LoopControl::Continue
            }
            _ => LoopControl::Continue,
        }
    }

    fn handle_replace(&mut self, key: Key) -> LoopControl {
        match key {
            Key::Escape | Key::Ctrl('c') | Key::Ctrl('[') => {
                self.leave_insert();
                LoopControl::Continue
            }
            Key::Enter => {
                self.split_line();
                LoopControl::Continue
            }
            Key::Backspace | Key::Ctrl('h') => {
                self.apply_motion(Motion::Left, 1);
                LoopControl::Continue
            }
            Key::Char(character) => {
                self.replace_char(character);
                LoopControl::Continue
            }
            Key::Tab => {
                self.replace_char('\t');
                LoopControl::Continue
            }
            other => self.handle_insert(other),
        }
    }

    fn handle_replace_char(&mut self, key: Key) -> LoopControl {
        self.mode = Mode::Normal;
        match key {
            Key::Escape | Key::Ctrl('c') | Key::Ctrl('[') => LoopControl::Continue,
            Key::Char(character) => {
                let repeats = self.take_count();
                self.push_undo();
                for _ in 0..repeats {
                    if self.line_char_count(self.cursor.row) == 0 {
                        self.beep = true;
                        break;
                    }
                    self.replace_char(character);
                }
                if !self.in_insert_like() && self.cursor.column > 0 {
                    self.cursor.column -= 1;
                }
                LoopControl::Continue
            }
            _ => {
                self.beep = true;
                LoopControl::Continue
            }
        }
    }

    fn handle_command(&mut self, key: Key) -> LoopControl {
        match key {
            Key::Escape | Key::Ctrl('c') | Key::Ctrl('[') => {
                self.command.clear();
                self.mode = Mode::Normal;
                LoopControl::Continue
            }
            Key::Enter => self.execute_ex(),
            Key::Backspace | Key::Ctrl('h') => {
                if self.command.pop().is_none() {
                    self.mode = Mode::Normal;
                }
                LoopControl::Continue
            }
            Key::Char(character) => {
                self.command.push(character);
                LoopControl::Continue
            }
            _ => LoopControl::Continue,
        }
    }

    fn enter_insert(&mut self, append: bool) -> LoopControl {
        self.push_undo();
        if append && self.line_char_count(self.cursor.row) > 0 {
            self.cursor.column += 1;
        }
        self.mode = Mode::Insert;
        self.reset_pending();
        LoopControl::Continue
    }

    fn leave_insert(&mut self) {
        self.mode = Mode::Normal;
        if self.cursor.column > 0 {
            self.cursor.column -= 1;
        }
        self.preferred_column = self.cursor.column;
    }

    fn open_line(&mut self, above: bool) -> LoopControl {
        self.push_undo();
        let row = if above { self.cursor.row } else { self.cursor.row + 1 };
        self.lines.insert(row, String::new());
        self.cursor.row = row;
        self.cursor.column = 0;
        self.preferred_column = 0;
        self.dirty = true;
        self.mode = Mode::Insert;
        self.reset_pending();
        LoopControl::Continue
    }

    fn insert_char(&mut self, character: char) {
        let row = self.cursor.row;
        let column = self.cursor.column;
        let line = &mut self.lines[row];
        let offset = char_to_byte(line, column);
        line.insert(offset, character);
        self.cursor.column += 1;
        self.preferred_column = self.cursor.column;
        self.dirty = true;
    }

    fn replace_char(&mut self, character: char) {
        let length = self.line_char_count(self.cursor.row);
        if self.cursor.column >= length {
            self.insert_char(character);
            return;
        }
        let line = &mut self.lines[self.cursor.row];
        let start = char_to_byte(line, self.cursor.column);
        let end = char_to_byte(line, self.cursor.column + 1);
        line.replace_range(start..end, character.encode_utf8(&mut [0; 4]));
        self.cursor.column += 1;
        self.preferred_column = self.cursor.column;
        self.dirty = true;
    }

    fn split_line(&mut self) {
        let row = self.cursor.row;
        let offset = char_to_byte(&self.lines[row], self.cursor.column);
        let rest = self.lines[row].split_off(offset);
        self.lines.insert(row + 1, rest);
        self.cursor.row = row + 1;
        self.cursor.column = 0;
        self.preferred_column = 0;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cursor.column > 0 {
            let line = &mut self.lines[self.cursor.row];
            let offset = char_to_byte(line, self.cursor.column - 1);
            line.remove(offset);
            self.cursor.column -= 1;
            self.preferred_column = self.cursor.column;
            self.dirty = true;
            return;
        }
        if self.cursor.row == 0 {
            self.beep = true;
            return;
        }
        let current = self.lines.remove(self.cursor.row);
        self.cursor.row -= 1;
        self.cursor.column = self.line_char_count(self.cursor.row);
        self.lines[self.cursor.row].push_str(&current);
        self.preferred_column = self.cursor.column;
        self.dirty = true;
    }

    fn backspace_word(&mut self) {
        if self.cursor.column == 0 {
            self.backspace();
            return;
        }
        let start = {
            let mut cursor = self.cursor;
            word_back(&self.lines, &mut cursor);
            if cursor.row != self.cursor.row {
                0
            } else {
                cursor.column
            }
        };
        let line = &mut self.lines[self.cursor.row];
        let from = char_to_byte(line, start);
        let to = char_to_byte(line, self.cursor.column);
        line.replace_range(from..to, "");
        self.cursor.column = start;
        self.preferred_column = start;
        self.dirty = true;
    }

    fn backspace_to_line_start(&mut self) {
        if self.cursor.column == 0 {
            return;
        }
        let line = &mut self.lines[self.cursor.row];
        let to = char_to_byte(line, self.cursor.column);
        line.replace_range(0..to, "");
        self.cursor.column = 0;
        self.preferred_column = 0;
        self.dirty = true;
    }

    fn delete_chars(&mut self, count: usize, before: bool) {
        self.push_undo();
        let mut yanked = String::new();
        for _ in 0..count {
            if before {
                if self.cursor.column == 0 {
                    break;
                }
                self.cursor.column -= 1;
            }
            let length = self.line_char_count(self.cursor.row);
            if length == 0 || self.cursor.column >= length {
                break;
            }
            let line = &mut self.lines[self.cursor.row];
            let offset = char_to_byte(line, self.cursor.column);
            yanked.push(line.remove(offset));
            self.dirty = true;
        }
        if yanked.is_empty() {
            self.beep = true;
            let _ = self.undo.pop();
            return;
        }
        self.register = Register::Chars(yanked);
        self.preferred_column = self.cursor.column;
    }

    fn join_lines(&mut self, count: usize) {
        let joins = count.max(1);
        if self.cursor.row + 1 >= self.lines.len() {
            self.beep = true;
            return;
        }
        self.push_undo();
        for _ in 0..joins {
            if self.cursor.row + 1 >= self.lines.len() {
                break;
            }
            let next = self.lines.remove(self.cursor.row + 1);
            let trimmed = next.trim_start();
            let current = &mut self.lines[self.cursor.row];
            if !current.is_empty() && !trimmed.is_empty() {
                current.push(' ');
            }
            current.push_str(trimmed);
            self.dirty = true;
        }
    }

    fn toggle_case(&mut self, count: usize) {
        self.push_undo();
        let mut changed = false;
        for _ in 0..count {
            let length = self.line_char_count(self.cursor.row);
            if length == 0 || self.cursor.column >= length {
                break;
            }
            let line = &mut self.lines[self.cursor.row];
            let start = char_to_byte(line, self.cursor.column);
            if let Some(character) = line[start..].chars().next() {
                let end = start + character.len_utf8();
                let replacement = if character.is_ascii_uppercase() {
                    character.to_ascii_lowercase()
                } else {
                    character.to_ascii_uppercase()
                };
                line.replace_range(start..end, replacement.encode_utf8(&mut [0; 4]));
                changed = true;
                self.dirty = true;
            }
            if self.cursor.column + 1 < length {
                self.cursor.column += 1;
            }
        }
        if !changed {
            self.beep = true;
            let _ = self.undo.pop();
        }
        self.preferred_column = self.cursor.column;
    }

    fn undo_last(&mut self) {
        match self.undo.pop() {
            Some(snapshot) => {
                self.lines = snapshot.lines;
                self.cursor = snapshot.cursor;
                self.dirty = snapshot.dirty;
                self.preferred_column = self.cursor.column;
                self.message = "1 change undone".to_string();
            }
            None => {
                self.beep = true;
                self.message = "Already at oldest change".to_string();
            }
        }
        self.reset_pending();
    }

    fn goto_line(&mut self, default_last: bool) {
        let row = if self.count == 0 {
            if default_last { self.lines.len() - 1 } else { 0 }
        } else {
            self.count.saturating_sub(1).min(self.lines.len() - 1)
        };
        self.count = 0;
        self.cursor.row = row;
        self.cursor.column = self.first_non_blank_at(row);
        self.preferred_column = self.cursor.column;
    }

    fn apply_motion(&mut self, motion: Motion, repeats: usize) {
        let insert_like = self.in_insert_like();
        for _ in 0..repeats {
            apply_motion_to(
                &self.lines,
                &mut self.cursor,
                &mut self.preferred_column,
                motion,
                insert_like,
            );
        }
        self.cursor.column = self.clamp_column(self.cursor.row, self.cursor.column);
    }

    fn apply_line_operator(&mut self, operator: Operator, count: usize) {
        self.push_undo();
        let start = self.cursor.row;
        let end = (start + count.saturating_sub(1)).min(self.lines.len() - 1);
        let yanked = self.lines[start..=end].to_vec();
        self.register = Register::Lines(yanked);
        match operator {
            Operator::Yank => {
                self.message = format!("{} line yanked", end - start + 1);
            }
            Operator::Delete => {
                self.lines.drain(start..=end);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor.row = start.min(self.lines.len() - 1);
                self.cursor.column = self.first_non_blank_at(self.cursor.row);
                self.dirty = true;
            }
            Operator::Change => {
                self.lines.drain(start..=end);
                self.lines.insert(start, String::new());
                self.cursor.row = start;
                self.cursor.column = 0;
                self.dirty = true;
                self.mode = Mode::Insert;
            }
        }
        self.operator = None;
        self.preferred_column = self.cursor.column;
    }

    fn apply_operator_motion(&mut self, operator: Operator, motion: Motion, repeats: usize) {
        let motion = match (operator, motion) {
            (Operator::Change, Motion::WordForward) => Motion::WordEnd,
            (_, motion) => motion,
        };
        let kind = motion_kind(motion);
        if matches!(kind, MotionKind::Linewise) {
            let start_row = self.cursor.row;
            let mut cursor = self.cursor;
            let mut preferred = self.preferred_column;
            for _ in 0..repeats {
                apply_motion_to(&self.lines, &mut cursor, &mut preferred, motion, false);
            }
            let end_row = cursor.row;
            let (top, bottom) = if start_row <= end_row { (start_row, end_row) } else { (end_row, start_row) };
            self.cursor.row = top;
            self.apply_line_operator(operator, bottom - top + 1);
            return;
        }
        let start = self.cursor;
        let mut end = start;
        let mut preferred = self.preferred_column;
        for _ in 0..repeats {
            apply_motion_to(&self.lines, &mut end, &mut preferred, motion, false);
        }
        let MotionKind::Charwise { inclusive } = kind else {
            return;
        };
        if inclusive {
            end = advance_char(&self.lines, end);
        }
        let (range_start, range_end) = order_cursors(start, end);
        if range_start == range_end {
            self.beep = true;
            self.operator = None;
            return;
        }
        self.push_undo();
        let yanked = yank_char_range(&self.lines, range_start, range_end);
        self.register = Register::Chars(yanked);
        if operator != Operator::Yank {
            delete_char_range(&mut self.lines, range_start, range_end);
            self.cursor = range_start;
            self.dirty = true;
        }
        if operator == Operator::Change {
            self.mode = Mode::Insert;
        }
        self.operator = None;
        self.preferred_column = self.cursor.column;
    }

    fn put(&mut self, before: bool) {
        match self.register.clone() {
            Register::Empty => self.beep = true,
            Register::Chars(text) => {
                if text.is_empty() {
                    self.beep = true;
                    return;
                }
                self.push_undo();
                if !before && self.line_char_count(self.cursor.row) > 0 {
                    self.cursor.column += 1;
                }
                if text.contains('\n') {
                    self.insert_multiline(&text);
                } else {
                    let line = &mut self.lines[self.cursor.row];
                    let offset = char_to_byte(line, self.cursor.column);
                    line.insert_str(offset, &text);
                    self.cursor.column += text.chars().count().saturating_sub(1);
                }
                self.preferred_column = self.cursor.column;
                self.dirty = true;
            }
            Register::Lines(lines) => {
                if lines.is_empty() {
                    self.beep = true;
                    return;
                }
                self.push_undo();
                let insert_at = if before { self.cursor.row } else { self.cursor.row + 1 };
                for (index, line) in lines.iter().enumerate() {
                    self.lines.insert(insert_at + index, line.clone());
                }
                self.cursor.row = insert_at.min(self.lines.len() - 1);
                self.cursor.column = self.first_non_blank_at(self.cursor.row);
                self.preferred_column = self.cursor.column;
                self.dirty = true;
            }
        }
        self.reset_pending();
    }

    fn insert_multiline(&mut self, text: &str) {
        let mut pieces: Vec<String> = text.split('\n').map(str::to_string).collect();
        if pieces.is_empty() {
            return;
        }
        let row = self.cursor.row;
        let offset = char_to_byte(&self.lines[row], self.cursor.column);
        let suffix = self.lines[row].split_off(offset);
        self.lines[row].push_str(&pieces.remove(0));
        let last_index = pieces.len();
        if last_index == 0 {
            self.lines[row].push_str(&suffix);
            return;
        }
        if let Some(last) = pieces.last_mut() {
            last.push_str(&suffix);
        }
        for (index, piece) in pieces.into_iter().enumerate() {
            self.lines.insert(row + 1 + index, piece);
        }
        self.cursor.row = row + last_index;
        self.cursor.column = self.line_char_count(self.cursor.row).saturating_sub(suffix.chars().count());
    }

    fn execute_ex(&mut self) -> LoopControl {
        let command = self.command.clone();
        self.command.clear();
        self.mode = Mode::Normal;
        match parse_ex(&command) {
            Ok(ExCommand::Quit { force }) => {
                if self.dirty && !force {
                    self.message = "No write since last change (add ! to override)".to_string();
                    self.beep = true;
                    LoopControl::Continue
                } else {
                    LoopControl::Quit
                }
            }
            Ok(ExCommand::Write { path, force, quit }) => {
                match self.write_buffer(path.as_deref(), force) {
                    Ok(()) if quit => LoopControl::Quit,
                    Ok(()) => LoopControl::Continue,
                    Err(message) => {
                        self.message = message;
                        self.beep = true;
                        LoopControl::Continue
                    }
                }
            }
            Ok(ExCommand::GoToLine(line)) => {
                self.count = line;
                self.goto_line(false);
                LoopControl::Continue
            }
            Ok(ExCommand::GoToLast) => {
                self.count = 0;
                self.goto_line(true);
                LoopControl::Continue
            }
            Err(message) => {
                self.message = message;
                self.beep = true;
                LoopControl::Continue
            }
        }
    }

    fn write_and_maybe_quit(&mut self, quit: bool) -> LoopControl {
        match self.write_buffer(None, false) {
            Ok(()) if quit => LoopControl::Quit,
            Ok(()) => LoopControl::Continue,
            Err(message) => {
                self.message = message;
                self.beep = true;
                LoopControl::Continue
            }
        }
    }

    fn write_buffer(&mut self, path: Option<&str>, force: bool) -> Result<(), String> {
        let target = match path.or(self.path.as_deref()) {
            Some(target) => target.to_string(),
            None => return Err("No file name".to_string()),
        };
        if self.readonly && !force && path.is_none() {
            return Err("'readonly' option is set (add ! to override)".to_string());
        }
        let text = self.to_text();
        std::fs::write(&target, text.as_bytes()).map_err(|err| format!("{target}: {err}"))?;
        if path.is_some() {
            self.path = Some(target.clone());
        }
        self.dirty = false;
        self.is_new_file = false;
        self.message = format!("\"{target}\" {}L, {}C written", self.lines.len(), text.len());
        Ok(())
    }
}

fn motion_from_key(key: Key) -> Option<Motion> {
    match key {
        Key::Char('h') | Key::Left | Key::Backspace | Key::Ctrl('h') => Some(Motion::Left),
        Key::Char('l') | Key::Right | Key::Char(' ') => Some(Motion::Right),
        Key::Char('j') | Key::Down | Key::Enter => Some(Motion::Down),
        Key::Char('k') | Key::Up => Some(Motion::Up),
        Key::Char('0') | Key::Home => Some(Motion::LineStart),
        Key::Char('^') => Some(Motion::FirstNonBlank),
        Key::Char('$') | Key::End => Some(Motion::LineEnd),
        Key::Char('w') => Some(Motion::WordForward),
        Key::Char('b') => Some(Motion::WordBack),
        Key::Char('e') => Some(Motion::WordEnd),
        Key::Char('G') => Some(Motion::FileEnd),
        _ => None,
    }
}

fn motion_kind(motion: Motion) -> MotionKind {
    match motion {
        Motion::Up | Motion::Down | Motion::FileStart | Motion::FileEnd => MotionKind::Linewise,
        Motion::WordEnd | Motion::LineEnd => MotionKind::Charwise { inclusive: true },
        _ => MotionKind::Charwise { inclusive: false },
    }
}

fn apply_motion_to(
    lines: &[String],
    cursor: &mut Cursor,
    preferred_column: &mut usize,
    motion: Motion,
    insert_like: bool,
) {
    match motion {
        Motion::Left => {
            if cursor.column > 0 {
                cursor.column -= 1;
            } else {
                // Stay; vi beeps at column 0.
            }
            *preferred_column = cursor.column;
        }
        Motion::Right => {
            let length = line_chars(lines, cursor.row);
            let max_column = if insert_like { length } else { length.saturating_sub(1) };
            if cursor.column < max_column {
                cursor.column += 1;
            }
            *preferred_column = cursor.column;
        }
        Motion::Up => {
            if cursor.row > 0 {
                cursor.row -= 1;
                cursor.column = clamp_column_for(lines, cursor.row, *preferred_column, insert_like);
            }
        }
        Motion::Down => {
            if cursor.row + 1 < lines.len() {
                cursor.row += 1;
                cursor.column = clamp_column_for(lines, cursor.row, *preferred_column, insert_like);
            }
        }
        Motion::LineStart => {
            cursor.column = 0;
            *preferred_column = 0;
        }
        Motion::FirstNonBlank => {
            cursor.column = first_non_blank(lines, cursor.row);
            *preferred_column = cursor.column;
        }
        Motion::LineEnd => {
            let length = line_chars(lines, cursor.row);
            cursor.column = if insert_like { length } else { length.saturating_sub(1) };
            *preferred_column = usize::MAX;
        }
        Motion::WordForward => word_forward(lines, cursor),
        Motion::WordBack => word_back(lines, cursor),
        Motion::WordEnd => word_end(lines, cursor),
        Motion::FileStart => {
            cursor.row = 0;
            cursor.column = first_non_blank(lines, 0);
            *preferred_column = cursor.column;
        }
        Motion::FileEnd => {
            cursor.row = lines.len().saturating_sub(1);
            cursor.column = first_non_blank(lines, cursor.row);
            *preferred_column = cursor.column;
        }
    }
}

fn line_chars(lines: &[String], row: usize) -> usize {
    lines.get(row).map(|line| line.chars().count()).unwrap_or(0)
}

fn first_non_blank(lines: &[String], row: usize) -> usize {
    match lines.get(row) {
        Some(line) => line.chars().position(|ch| !ch.is_whitespace()).unwrap_or(line.chars().count()),
        None => 0,
    }
}

fn clamp_column_for(lines: &[String], row: usize, preferred: usize, insert_like: bool) -> usize {
    let length = line_chars(lines, row);
    if insert_like {
        preferred.min(length)
    } else if length == 0 {
        0
    } else {
        preferred.min(length - 1)
    }
}

fn char_at(lines: &[String], cursor: Cursor) -> Option<char> {
    lines.get(cursor.row)?.chars().nth(cursor.column)
}

fn advance_position(lines: &[String], cursor: &mut Cursor) -> bool {
    let length = line_chars(lines, cursor.row);
    if length > 0 && cursor.column + 1 < length {
        cursor.column += 1;
        return true;
    }
    if cursor.row + 1 < lines.len() {
        cursor.row += 1;
        cursor.column = 0;
        return true;
    }
    if length > 0 && cursor.column < length.saturating_sub(1) {
        cursor.column = length - 1;
    }
    false
}

fn retreat_position(lines: &[String], cursor: &mut Cursor) -> bool {
    if cursor.column > 0 {
        cursor.column -= 1;
        return true;
    }
    if cursor.row == 0 {
        return false;
    }
    cursor.row -= 1;
    cursor.column = line_chars(lines, cursor.row).saturating_sub(1);
    true
}

fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn skip_while(lines: &[String], cursor: &mut Cursor, predicate: impl Fn(char) -> bool, forward: bool) {
    loop {
        let Some(character) = char_at(lines, *cursor) else {
            break;
        };
        if !predicate(character) {
            break;
        }
        let moved = if forward { advance_position(lines, cursor) } else { retreat_position(lines, cursor) };
        if !moved {
            break;
        }
    }
}

fn word_forward(lines: &[String], cursor: &mut Cursor) {
    if let Some(character) = char_at(lines, *cursor) {
        if is_word_char(character) {
            skip_while(lines, cursor, is_word_char, true);
        } else if !character.is_whitespace() {
            skip_while(lines, cursor, |ch| !ch.is_whitespace() && !is_word_char(ch), true);
        }
    } else if cursor.row + 1 < lines.len() {
        cursor.row += 1;
        cursor.column = 0;
    }
    loop {
        match char_at(lines, *cursor) {
            Some(character) if character.is_whitespace() => {
                if !advance_position(lines, cursor) {
                    break;
                }
            }
            None => {
                if cursor.row + 1 < lines.len() {
                    cursor.row += 1;
                    cursor.column = 0;
                } else {
                    let length = line_chars(lines, cursor.row);
                    if length > 0 {
                        cursor.column = length - 1;
                    }
                    break;
                }
            }
            Some(_) => break,
        }
    }
}

fn word_back(lines: &[String], cursor: &mut Cursor) {
    let _ = retreat_position(lines, cursor);
    loop {
        match char_at(lines, *cursor) {
            Some(character) if character.is_whitespace() => {
                if !retreat_position(lines, cursor) {
                    return;
                }
            }
            None => {
                if cursor.row == 0 {
                    cursor.column = 0;
                    return;
                }
                if !retreat_position(lines, cursor) {
                    return;
                }
            }
            Some(_) => break,
        }
    }
    if let Some(character) = char_at(lines, *cursor) {
        if is_word_char(character) {
            loop {
                if cursor.column == 0 {
                    break;
                }
                let mut previous = *cursor;
                previous.column -= 1;
                match char_at(lines, previous) {
                    Some(previous_char) if is_word_char(previous_char) => *cursor = previous,
                    _ => break,
                }
            }
        } else {
            loop {
                if cursor.column == 0 {
                    break;
                }
                let mut previous = *cursor;
                previous.column -= 1;
                match char_at(lines, previous) {
                    Some(previous_char) if !previous_char.is_whitespace() && !is_word_char(previous_char) => {
                        *cursor = previous
                    }
                    _ => break,
                }
            }
        }
    }
}

fn word_end(lines: &[String], cursor: &mut Cursor) {
    let current = char_at(lines, *cursor);
    let next = {
        let mut peek = *cursor;
        if advance_position(lines, &mut peek) { char_at(lines, peek) } else { None }
    };
    let already_at_end = match (current, next) {
        (Some(left), Some(right)) => {
            (is_word_char(left) && !is_word_char(right))
                || (!is_word_char(left) && !left.is_whitespace() && (is_word_char(right) || right.is_whitespace()))
        }
        (Some(_), None) => true,
        _ => false,
    };
    if already_at_end {
        let _ = advance_position(lines, cursor);
    }
    loop {
        match char_at(lines, *cursor) {
            Some(character) if character.is_whitespace() => {
                if !advance_position(lines, cursor) {
                    return;
                }
            }
            None => {
                if !advance_position(lines, cursor) {
                    return;
                }
            }
            Some(_) => break,
        }
    }
    if let Some(character) = char_at(lines, *cursor) {
        if is_word_char(character) {
            loop {
                let mut peek = *cursor;
                if !advance_position(lines, &mut peek) {
                    break;
                }
                match char_at(lines, peek) {
                    Some(next_char) if is_word_char(next_char) => *cursor = peek,
                    _ => break,
                }
            }
        } else {
            loop {
                let mut peek = *cursor;
                if !advance_position(lines, &mut peek) {
                    break;
                }
                match char_at(lines, peek) {
                    Some(next_char) if !next_char.is_whitespace() && !is_word_char(next_char) => *cursor = peek,
                    _ => break,
                }
            }
        }
    }
}

fn order_cursors(left: Cursor, right: Cursor) -> (Cursor, Cursor) {
    if left.row < right.row || (left.row == right.row && left.column <= right.column) {
        (left, right)
    } else {
        (right, left)
    }
}

fn advance_char(lines: &[String], cursor: Cursor) -> Cursor {
    let mut next = cursor;
    let _ = advance_position(lines, &mut next);
    if next == cursor {
        let length = line_chars(lines, cursor.row);
        Cursor { row: cursor.row, column: length }
    } else {
        next
    }
}

fn yank_char_range(lines: &[String], start: Cursor, end: Cursor) -> String {
    if start.row == end.row {
        let line = &lines[start.row];
        let from = char_to_byte(line, start.column);
        let to = char_to_byte(line, end.column);
        return line.get(from..to).unwrap_or("").to_string();
    }
    let mut yanked = String::new();
    let first = &lines[start.row];
    yanked.push_str(&first[char_to_byte(first, start.column)..]);
    yanked.push('\n');
    for row in (start.row + 1)..end.row {
        yanked.push_str(&lines[row]);
        yanked.push('\n');
    }
    let last = &lines[end.row];
    yanked.push_str(&last[..char_to_byte(last, end.column)]);
    yanked
}

fn delete_char_range(lines: &mut Vec<String>, start: Cursor, end: Cursor) {
    if start.row == end.row {
        let line = &mut lines[start.row];
        let from = char_to_byte(line, start.column);
        let to = char_to_byte(line, end.column);
        line.replace_range(from..to, "");
        return;
    }
    let prefix = {
        let first = &lines[start.row];
        first[..char_to_byte(first, start.column)].to_string()
    };
    let suffix = {
        let last = &lines[end.row];
        last[char_to_byte(last, end.column)..].to_string()
    };
    lines.drain(start.row..=end.row);
    let mut combined = prefix;
    combined.push_str(&suffix);
    lines.insert(start.row, combined);
}

fn char_to_byte(text: &str, index: usize) -> usize {
    match text.char_indices().nth(index) {
        Some((offset, _)) => offset,
        None => text.len(),
    }
}

fn lines_from_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> =
        text.split('\n').map(|line| line.trim_end_matches('\r').to_string()).collect();
    if text.ends_with('\n') {
        let _ = lines.pop();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn text_from_lines(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn parse_ex(input: &str) -> Result<ExCommand, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Empty command".to_string());
    }
    if trimmed == "$" {
        return Ok(ExCommand::GoToLast);
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        let line = trimmed.parse::<usize>().unwrap_or(0);
        return Ok(ExCommand::GoToLine(line.max(1)));
    }
    let mut parts = trimmed.split_whitespace();
    let name_raw = parts.next().unwrap_or("");
    let force = name_raw.ends_with('!');
    let name = if force { &name_raw[..name_raw.len() - 1] } else { name_raw };
    let argument = parts.next().map(str::to_string);
    if parts.next().is_some() {
        return Err("Trailing characters".to_string());
    }
    match name {
        "q" | "quit" => Ok(ExCommand::Quit { force }),
        "w" | "write" => Ok(ExCommand::Write { path: argument, force, quit: false }),
        "wq" | "x" | "xit" => Ok(ExCommand::Write { path: argument, force, quit: true }),
        _ => Err(format!("Not an editor command: {trimmed}")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct View {
    lines: Vec<String>,
    status: String,
    cursor_row: usize,
    cursor_column: usize,
    cursor_on_status: bool,
}

fn format_view(editor: &Editor) -> View {
    let columns = editor.view_columns.max(1);
    let content_rows = editor.content_rows();
    let origin = editor.view_origin;
    let hscroll = if editor.mode == Mode::Command {
        0
    } else if editor.cursor.column >= columns {
        editor.cursor.column + 1 - columns
    } else {
        0
    };
    let mut lines = Vec::with_capacity(content_rows);
    for offset in 0..content_rows {
        let row = origin + offset;
        if row < editor.lines.len() {
            let displayed: String = editor.lines[row].chars().skip(hscroll).take(columns).collect();
            lines.push(displayed);
        } else {
            lines.push("~".to_string());
        }
    }
    let cursor_on_status = editor.mode == Mode::Command;
    let (cursor_row, cursor_column) = if cursor_on_status {
        (content_rows.saturating_sub(1), 1 + editor.command.chars().count())
    } else {
        (editor.cursor.row.saturating_sub(origin), editor.cursor.column.saturating_sub(hscroll))
    };
    View {
        lines,
        status: format_status(editor, columns),
        cursor_row,
        cursor_column,
        cursor_on_status,
    }
}

fn format_status(editor: &Editor, columns: usize) -> String {
    let left = match editor.mode {
        Mode::Insert => "-- INSERT --".to_string(),
        Mode::Replace => "-- REPLACE --".to_string(),
        Mode::Command => format!(":{}", editor.command),
        _ if !editor.message.is_empty() => editor.message.clone(),
        _ => file_label(editor),
    };
    if editor.mode == Mode::Command {
        return truncate_chars(&left, columns);
    }
    let right = format!("{}:{}", editor.cursor.row + 1, editor.cursor.column + 1);
    if left.chars().count() + 1 + right.chars().count() >= columns {
        return truncate_chars(&left, columns);
    }
    let gap = columns.saturating_sub(left.chars().count() + right.chars().count());
    let mut status = left;
    for _ in 0..gap {
        status.push(' ');
    }
    status.push_str(&right);
    truncate_chars(&status, columns)
}

fn file_label(editor: &Editor) -> String {
    let mut label = match &editor.path {
        Some(path) => format!("\"{path}\""),
        None => "[No Name]".to_string(),
    };
    if editor.dirty {
        label.push_str(" [Modified]");
    } else if editor.is_new_file {
        label.push_str(" [New File]");
    }
    if editor.readonly {
        label.push_str(" [readonly]");
    }
    label
}

fn truncate_chars(text: &str, columns: usize) -> String {
    text.chars().take(columns).collect()
}

/// POSIX termios raw mode. Absent on the Secular serial console: `isatty` is
/// true, but `tcgetattr` returns ENOTTY because fdio does not implement TCGETS.
struct PosixRawMode {
    fd: i32,
    original: libc::termios,
}

impl PosixRawMode {
    fn enter() -> Option<Self> {
        let fd = libc::STDIN_FILENO;
        let mut original = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `tcgetattr` writes a complete `termios` into `original` on success.
        // The pointer is exclusive, correctly aligned, and not retained by libc.
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: `tcgetattr` succeeded, so every field of `termios` is initialized.
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        // SAFETY: `raw` is a valid `termios` copied from the fd; `cfmakeraw` only
        // mutates flag bits and control characters in place.
        unsafe { libc::cfmakeraw(&mut raw) };
        raw.c_oflag |= libc::OPOST | libc::ONLCR;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: `raw` is a fully initialized `termios` derived from the current
        // attributes. TCSADRAIN drains pending output and does not retain the pointer.
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

impl Drop for PosixRawMode {
    fn drop(&mut self) {
        // SAFETY: `original` was captured from a successful `tcgetattr` on this fd
        // and remains a valid termios snapshot for the lifetime of the guard.
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.original) };
    }
}

/// Fuchsia PTY raw mode. `FEATURE_RAW` delivers Ctrl-C as a byte instead of a
/// PTY interrupt, so zxsh will not kill this process while the editor runs.
#[cfg(target_os = "fuchsia")]
struct PtyRawMode {
    proxy: fpty::DeviceSynchronousProxy,
}

#[cfg(target_os = "fuchsia")]
impl PtyRawMode {
    fn enter() -> Option<Self> {
        let channel = fdio::clone_channel(&io::stdin()).ok()?;
        let proxy = fpty::DeviceSynchronousProxy::new(channel);
        let (status, _) =
            proxy.clr_set_feature(0, fpty::FEATURE_RAW, zx::MonotonicInstant::INFINITE).ok()?;
        zx::Status::ok(status).ok()?;
        Some(Self { proxy })
    }
}

#[cfg(target_os = "fuchsia")]
impl Drop for PtyRawMode {
    fn drop(&mut self) {
        let _ = self.proxy.clr_set_feature(fpty::FEATURE_RAW, 0, zx::MonotonicInstant::INFINITE);
    }
}

/// Held for the editor session. POSIX termios is used when the fd supports it.
/// The Secular console does not; input is already uncooked, and `PtyRawMode`
/// covers Ctrl-C. Either guard may be `None`.
struct RawTerminal {
    _posix: Option<PosixRawMode>,
    #[cfg(target_os = "fuchsia")]
    _pty: Option<PtyRawMode>,
}

impl RawTerminal {
    fn enter() -> Self {
        Self {
            _posix: PosixRawMode::enter(),
            #[cfg(target_os = "fuchsia")]
            _pty: PtyRawMode::enter(),
        }
    }
}

struct Screen {
    drawn_rows: usize,
    last_cursor_row: usize,
    last_cursor_on_status: bool,
}

impl Screen {
    fn new() -> Self {
        Self { drawn_rows: 0, last_cursor_row: 0, last_cursor_on_status: true }
    }
}

fn terminal_size() -> (usize, usize) {
    let mut size = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes into a local `winsize`. `size` is aligned, exclusive,
    // and not retained. Unsigned fields accept any bit pattern returned by the ioctl.
    let got_size = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    let columns = if got_size && size.ws_col > 0 { size.ws_col as usize } else { DEFAULT_COLUMNS };
    let rows = if got_size && size.ws_row > 0 { size.ws_row as usize } else { DEFAULT_TERMINAL_ROWS };
    (columns, rows)
}

fn read_stdin_byte() -> io::Result<Option<u8>> {
    let mut buffer = [0u8; 1];
    loop {
        // SAFETY: `buffer` is a writable 1-byte stack array owned for the duration of
        // the call. `read` does not retain the pointer.
        let result =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr() as *mut libc::c_void, 1) };
        if result < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if result == 0 {
            return Ok(None);
        }
        return Ok(Some(buffer[0]));
    }
}

fn stdin_ready(timeout_ms: i32) -> bool {
    let mut fds = [libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 }];
    // SAFETY: `fds` holds one valid `pollfd` for this fd. `poll` reads/writes
    // `revents` only and does not retain the pointer after it returns.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) > 0 }
}

fn utf8_width(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first & 0xe0 == 0xc0 {
        2
    } else if first & 0xf0 == 0xe0 {
        3
    } else if first & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

fn decode_utf8_char(first: u8, rest: &[u8]) -> char {
    let width = utf8_width(first);
    let mut bytes = [0u8; 4];
    bytes[0] = first;
    for index in 1..width {
        bytes[index] = rest.get(index - 1).copied().unwrap_or(0x80);
    }
    std::str::from_utf8(&bytes[..width]).ok().and_then(|text| text.chars().next()).unwrap_or('\u{FFFD}')
}

fn decode_key_from_bytes(bytes: &[u8]) -> Option<(Key, usize)> {
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0];
    match first {
        0x1b => Some(decode_escape(&bytes[1..])),
        b'\r' | b'\n' => Some((Key::Enter, 1)),
        0x08 | 0x7f => Some((Key::Backspace, 1)),
        b'\t' => Some((Key::Tab, 1)),
        0x01..=0x1a => Some((Key::Ctrl((b'a' + (first - 1)) as char), 1)),
        0x1c..=0x1f => Some((Key::Ctrl((b'a' + (first - 1)) as char), 1)),
        byte if byte < 0x80 => Some((Key::Char(byte as char), 1)),
        byte => {
            let width = utf8_width(byte).min(bytes.len());
            Some((Key::Char(decode_utf8_char(byte, &bytes[1..width])), width))
        }
    }
}

fn decode_escape(rest: &[u8]) -> (Key, usize) {
    if rest.is_empty() {
        return (Key::Escape, 1);
    }
    if rest[0] == b'[' {
        return decode_csi(rest);
    }
    if rest[0] == b'O' && rest.len() >= 2 {
        return match rest[1] {
            b'A' => (Key::Up, 3),
            b'B' => (Key::Down, 3),
            b'C' => (Key::Right, 3),
            b'D' => (Key::Left, 3),
            b'H' => (Key::Home, 3),
            b'F' => (Key::End, 3),
            _ => (Key::Escape, 1),
        };
    }
    (Key::Escape, 1)
}

fn decode_csi(rest: &[u8]) -> (Key, usize) {
    // `rest` starts at `[`.
    if rest.len() >= 2 {
        match rest[1] {
            b'A' => return (Key::Up, 3),
            b'B' => return (Key::Down, 3),
            b'C' => return (Key::Right, 3),
            b'D' => return (Key::Left, 3),
            b'H' => return (Key::Home, 3),
            b'F' => return (Key::End, 3),
            b'3' if rest.len() >= 3 && rest[2] == b'~' => return (Key::Delete, 4),
            b'1' if rest.len() >= 3 && rest[2] == b'~' => return (Key::Home, 4),
            b'4' if rest.len() >= 3 && rest[2] == b'~' => return (Key::End, 4),
            _ => {}
        }
    }
    (Key::Escape, 1)
}

fn read_key() -> io::Result<Option<Key>> {
    let Some(first) = read_stdin_byte()? else {
        return Ok(None);
    };
    if first != 0x1b {
        if first < 0x80 {
            return Ok(decode_key_from_bytes(&[first]).map(|(key, _)| key));
        }
        let width = utf8_width(first);
        let mut bytes = vec![first];
        for _ in 1..width {
            match read_stdin_byte()? {
                Some(byte) => bytes.push(byte),
                None => break,
            }
        }
        return Ok(decode_key_from_bytes(&bytes).map(|(key, _)| key));
    }
    if !stdin_ready(ESCAPE_TIMEOUT_MS) {
        return Ok(Some(Key::Escape));
    }
    let mut bytes = vec![0x1b];
    while bytes.len() < 8 && stdin_ready(0) {
        match read_stdin_byte()? {
            Some(byte) => bytes.push(byte),
            None => break,
        }
    }
    if bytes.len() == 1 {
        if let Some(byte) = read_stdin_byte()? {
            bytes.push(byte);
        }
    }
    Ok(decode_key_from_bytes(&bytes).map(|(key, _)| key))
}

fn query_cursor_row() -> Option<usize> {
    let mut stdout = io::stdout();
    stdout.write_all(QUERY_CURSOR).ok()?;
    stdout.flush().ok()?;
    let mut bytes = Vec::new();
    for _ in 0..32 {
        let timeout_ms = if bytes.is_empty() { 200 } else { 50 };
        if !stdin_ready(timeout_ms) {
            return None;
        }
        let byte = read_stdin_byte().ok()??;
        bytes.push(byte);
        if byte == b'R' {
            break;
        }
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let text = text.strip_prefix("\x1b[")?;
    let text = text.strip_suffix('R')?;
    let row = text.split(';').next()?.parse::<usize>().ok()?;
    Some(row.max(1))
}

fn choose_view_rows(terminal_rows: usize, cursor_row: Option<usize>) -> usize {
    let remaining = match cursor_row {
        Some(row) if row <= terminal_rows => terminal_rows.saturating_sub(row).saturating_add(1),
        _ => DEFAULT_VIEW_ROWS,
    };
    remaining.clamp(MIN_VIEW_ROWS, terminal_rows.max(MIN_VIEW_ROWS))
}

fn write_up<W: Write>(writer: &mut W, count: usize) -> io::Result<()> {
    if count > 0 {
        write!(writer, "\x1b[{count}A")?;
    }
    Ok(())
}

fn write_down<W: Write>(writer: &mut W, count: usize) -> io::Result<()> {
    if count > 0 {
        write!(writer, "\x1b[{count}B")?;
    }
    Ok(())
}

fn write_forward<W: Write>(writer: &mut W, count: usize) -> io::Result<()> {
    if count > 0 {
        write!(writer, "\x1b[{count}C")?;
    }
    Ok(())
}

fn draw_view<W: Write>(writer: &mut W, view: &View, screen: &mut Screen) -> io::Result<()> {
    writer.write_all(HIDE_CURSOR)?;
    if screen.drawn_rows > 0 {
        if screen.last_cursor_on_status {
            write_up(writer, screen.drawn_rows.saturating_sub(1))?;
        } else {
            write_up(writer, screen.last_cursor_row)?;
        }
    }
    for line in &view.lines {
        writer.write_all(ERASE_LINE)?;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    writer.write_all(ERASE_LINE)?;
    writer.write_all(view.status.as_bytes())?;
    if view.cursor_on_status {
        writer.write_all(b"\r")?;
        write_forward(writer, view.cursor_column)?;
    } else {
        let up = view.lines.len() - view.cursor_row;
        write_up(writer, up)?;
        writer.write_all(b"\r")?;
        write_forward(writer, view.cursor_column)?;
    }
    writer.write_all(SHOW_CURSOR)?;
    writer.flush()?;
    screen.drawn_rows = view.lines.len() + 1;
    screen.last_cursor_row = view.cursor_row;
    screen.last_cursor_on_status = view.cursor_on_status;
    Ok(())
}

fn leave_view<W: Write>(writer: &mut W, screen: &Screen) -> io::Result<()> {
    if screen.drawn_rows == 0 {
        writer.write_all(SHOW_CURSOR)?;
        return writer.flush();
    }
    if !screen.last_cursor_on_status {
        let down = screen.drawn_rows.saturating_sub(1).saturating_sub(screen.last_cursor_row);
        write_down(writer, down)?;
    }
    writer.write_all(SHOW_CURSOR)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn load_editor(path: Option<String>, readonly: bool) -> Result<Editor, String> {
    let Some(path) = path else {
        return Ok(Editor::from_text(None, "", readonly, true));
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes.contains(&0) {
                return Err(format!("cope: {path}: Binary file"));
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("cope: {path}: Invalid UTF-8"))?;
            let byte_count = text.len();
            let mut editor = Editor::from_text(Some(path), &text, readonly, false);
            editor.set_open_message(byte_count);
            Ok(editor)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            Ok(Editor::from_text(Some(path), "", readonly, true))
        }
        Err(err) => Err(format!("cope: {path}: {err}")),
    }
}

fn run_editor(path: Option<String>, readonly: bool) -> i32 {
    let mut editor = match load_editor(path, readonly) {
        Ok(editor) => editor,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        let _ = writeln!(io::stderr(), "cope: not a terminal");
        return 1;
    }
    let _raw = RawTerminal::enter();
    let (columns, terminal_rows) = terminal_size();
    let cursor_row = query_cursor_row();
    editor.set_view_size(columns, choose_view_rows(terminal_rows, cursor_row));
    let mut stdout = io::stdout().lock();
    let mut screen = Screen::new();
    let mut status = 0;
    let result = (|| -> io::Result<()> {
        loop {
            let view = format_view(&editor);
            draw_view(&mut stdout, &view, &mut screen)?;
            if editor.beep {
                stdout.write_all(BELL)?;
                stdout.flush()?;
            }
            match read_key()? {
                None => {
                    if editor.dirty {
                        status = 1;
                        editor.message = "No write since last change".to_string();
                        let view = format_view(&editor);
                        draw_view(&mut stdout, &view, &mut screen)?;
                        leave_view(&mut stdout, &screen)?;
                        drop(stdout);
                        let _ = writeln!(io::stderr(), "cope: no write since last change");
                        return Ok(());
                    }
                    break;
                }
                Some(key) => {
                    if editor.handle_key(key) == LoopControl::Quit {
                        break;
                    }
                }
            }
        }
        leave_view(&mut stdout, &screen)?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = io::stdout().lock().write_all(SHOW_CURSOR);
        let _ = writeln!(io::stderr(), "cope: {err}");
        return 1;
    }
    status
}

fn run(args: &[String]) -> i32 {
    match parse_args(args) {
        Ok(Options::Help) => {
            let _ = write!(io::stdout(), "{HELP}");
            0
        }
        Ok(Options::Edit { path, readonly }) => run_editor(path, readonly),
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            1
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        Editor::from_text(None, text, false, text.is_empty())
    }

    fn type_chars(session: &mut Editor, chars: &str) {
        for character in chars.chars() {
            let key = match character {
                '\n' => Key::Enter,
                '\u{1b}' => Key::Escape,
                value => Key::Char(value),
            };
            let _ = session.handle_key(key);
        }
    }

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert_eq!(parse_args(&args).unwrap(), Options::Edit { path: None, readonly: false });
    }

    #[test]
    fn parse_args_file_and_readonly() {
        let args = ["-R".to_string(), "notes.txt".to_string()];
        assert_eq!(
            parse_args(&args).unwrap(),
            Options::Edit { path: Some("notes.txt".to_string()), readonly: true }
        );
    }

    #[test]
    fn parse_args_help() {
        let args = ["-h".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options::Help);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_extra_operand() {
        let args = ["a".to_string(), "b".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn parse_ex_write_quit_and_line() {
        assert_eq!(
            parse_ex("w").unwrap(),
            ExCommand::Write { path: None, force: false, quit: false }
        );
        assert_eq!(
            parse_ex("w! /tmp/out").unwrap(),
            ExCommand::Write { path: Some("/tmp/out".to_string()), force: true, quit: false }
        );
        assert_eq!(parse_ex("q!").unwrap(), ExCommand::Quit { force: true });
        assert_eq!(
            parse_ex("wq").unwrap(),
            ExCommand::Write { path: None, force: false, quit: true }
        );
        assert_eq!(parse_ex("12").unwrap(), ExCommand::GoToLine(12));
        assert_eq!(parse_ex("$").unwrap(), ExCommand::GoToLast);
    }

    #[test]
    fn lines_from_text_strips_trailing_newline() {
        assert_eq!(lines_from_text(""), vec![""]);
        assert_eq!(lines_from_text("a\n"), vec!["a"]);
        assert_eq!(lines_from_text("a\nb\n"), vec!["a", "b"]);
        assert_eq!(lines_from_text("a\nb"), vec!["a", "b"]);
        assert_eq!(text_from_lines(&["a".to_string(), "b".to_string()]), "a\nb\n");
    }

    #[test]
    fn insert_then_escape_leaves_normal_mode() {
        let mut session = editor("");
        type_chars(&mut session, "ihello\u{1b}");
        assert_eq!(session.mode, Mode::Normal);
        assert_eq!(session.lines, vec!["hello".to_string()]);
        assert_eq!(session.cursor, Cursor { row: 0, column: 4 });
        assert!(session.dirty);
    }

    #[test]
    fn motions_hjkl_and_line_ends() {
        let mut session = editor("ab\ncd\n");
        type_chars(&mut session, "l");
        assert_eq!(session.cursor, Cursor { row: 0, column: 1 });
        type_chars(&mut session, "j");
        assert_eq!(session.cursor, Cursor { row: 1, column: 1 });
        type_chars(&mut session, "0");
        assert_eq!(session.cursor.column, 0);
        type_chars(&mut session, "$");
        assert_eq!(session.cursor.column, 1);
        type_chars(&mut session, "gg");
        assert_eq!(session.cursor.row, 0);
        type_chars(&mut session, "G");
        assert_eq!(session.cursor.row, 1);
    }

    #[test]
    fn word_forward_and_back() {
        let mut session = editor("hello world");
        type_chars(&mut session, "w");
        assert_eq!(session.cursor, Cursor { row: 0, column: 6 });
        type_chars(&mut session, "b");
        assert_eq!(session.cursor, Cursor { row: 0, column: 0 });
        type_chars(&mut session, "e");
        assert_eq!(session.cursor, Cursor { row: 0, column: 4 });
    }

    #[test]
    fn delete_line_and_put() {
        let mut session = editor("one\ntwo\nthree\n");
        type_chars(&mut session, "dd");
        assert_eq!(session.lines, vec!["two".to_string(), "three".to_string()]);
        type_chars(&mut session, "p");
        assert_eq!(session.lines, vec!["two".to_string(), "one".to_string(), "three".to_string()]);
    }

    #[test]
    fn delete_word_and_undo() {
        let mut session = editor("hello world");
        type_chars(&mut session, "dw");
        assert_eq!(session.lines, vec!["world".to_string()]);
        type_chars(&mut session, "u");
        assert_eq!(session.lines, vec!["hello world".to_string()]);
    }

    #[test]
    fn count_deletes_two_lines() {
        let mut session = editor("a\nb\nc\n");
        type_chars(&mut session, "2dd");
        assert_eq!(session.lines, vec!["c".to_string()]);
    }

    #[test]
    fn open_line_and_join() {
        let mut session = editor("alpha");
        type_chars(&mut session, "o");
        type_chars(&mut session, "beta\u{1b}");
        assert_eq!(session.lines, vec!["alpha".to_string(), "beta".to_string()]);
        type_chars(&mut session, "kJ");
        assert_eq!(session.lines, vec!["alpha beta".to_string()]);
    }

    #[test]
    fn quit_refuses_dirty_buffer() {
        let mut session = editor("");
        type_chars(&mut session, "ix\u{1b}:q\n");
        assert_eq!(session.mode, Mode::Normal);
        assert!(session.dirty);
        assert!(session.message.contains("No write since last change"));
        let control = session.handle_key(Key::Char(':'));
        assert_eq!(control, LoopControl::Continue);
        type_chars(&mut session, "q!\n");
        // `q!` is executed through handle_command Enter.
    }

    #[test]
    fn force_quit_discards_changes() {
        let mut session = editor("");
        type_chars(&mut session, "ix\u{1b}");
        session.command = "q!".to_string();
        session.mode = Mode::Command;
        assert_eq!(session.handle_key(Key::Enter), LoopControl::Quit);
    }

    #[test]
    fn zz_writes_when_path_is_set() {
        let directory = std::env::temp_dir();
        let path = directory.join("cope-zz-test.txt");
        let path_string = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        let mut session = Editor::from_text(Some(path_string.clone()), "", false, true);
        type_chars(&mut session, "ihi\u{1b}");
        assert_eq!(session.handle_key(Key::Char('Z')), LoopControl::Continue);
        assert_eq!(session.handle_key(Key::Char('Z')), LoopControl::Quit);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "hi\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn format_view_uses_tildes_and_insert_status() {
        let mut session = editor("hello");
        session.set_view_size(20, 8);
        type_chars(&mut session, "i");
        let view = format_view(&session);
        assert_eq!(view.lines[0], "hello");
        assert!(view.lines[1].starts_with('~'));
        assert!(view.status.contains("INSERT"));
        assert!(!view.cursor_on_status);
    }

    #[test]
    fn decode_arrow_keys_and_escape() {
        assert_eq!(decode_key_from_bytes(b"\x1b[A"), Some((Key::Up, 3)));
        assert_eq!(decode_key_from_bytes(b"\x1b[3~"), Some((Key::Delete, 4)));
        assert_eq!(decode_key_from_bytes(b"\x1b"), Some((Key::Escape, 1)));
        assert_eq!(decode_key_from_bytes(b"\x03"), Some((Key::Ctrl('c'), 1)));
        assert_eq!(decode_key_from_bytes("é".as_bytes()).map(|(key, _)| key), Some(Key::Char('é')));
    }

    #[test]
    fn choose_view_rows_keeps_prompt_visible() {
        assert_eq!(choose_view_rows(24, Some(20)), 8);
        assert_eq!(choose_view_rows(24, Some(1)), 24);
        assert_eq!(choose_view_rows(24, None), DEFAULT_VIEW_ROWS);
    }

    #[test]
    fn replace_one_character() {
        let mut session = editor("cat");
        type_chars(&mut session, "rb");
        assert_eq!(session.lines, vec!["bat".to_string()]);
        assert_eq!(session.mode, Mode::Normal);
    }

    #[test]
    fn change_word_enters_insert() {
        let mut session = editor("hello world");
        type_chars(&mut session, "cw");
        assert_eq!(session.mode, Mode::Insert);
        assert_eq!(session.lines, vec![" world".to_string()]);
    }
}
