// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! List contents of directories in a tree-like format.

use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "Usage: tree [-adfFS] [-L level] [directory...]";
const ERROR_OPENING_DIR: &str = " [error opening dir]";
const DEFAULT_ROOT: &str = ".";

const UTF8_GRAPHICS: LineGraphics =
    LineGraphics { branch: "├── ", last: "└── ", vertical: "│   ", empty: "    " };

const ASCII_GRAPHICS: LineGraphics =
    LineGraphics { branch: "|-- ", last: "`-- ", vertical: "|   ", empty: "    " };

struct LineGraphics {
    branch: &'static str,
    last: &'static str,
    vertical: &'static str,
    empty: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Options {
    all: bool,
    dirs_only: bool,
    classify: bool,
    full_path: bool,
    ascii: bool,
    max_level: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Summary {
    directories: u64,
    files: u64,
}

impl Summary {
    fn add(&mut self, other: Summary) {
        self.directories = self.directories.saturating_add(other.directories);
        self.files = self.files.saturating_add(other.files);
    }
}

struct Child {
    name: String,
    path: PathBuf,
    is_dir: bool,
    is_symlink: bool,
    is_executable: bool,
}

struct WalkResult {
    summary: Summary,
    had_error: bool,
}

fn graphics(options: &Options) -> &'static LineGraphics {
    if options.ascii { &ASCII_GRAPHICS } else { &UTF8_GRAPHICS }
}

fn can_descend(level: u32, max_level: Option<u32>) -> bool {
    match max_level {
        None => true,
        Some(max) => level < max,
    }
}

fn classify_suffix(child: &Child, options: &Options) -> &'static str {
    if !options.classify {
        return "";
    }
    if child.is_dir {
        "/"
    } else if child.is_symlink {
        "@"
    } else if child.is_executable {
        "*"
    } else {
        ""
    }
}

fn child_label(parent_label: &str, name: &str, full_path: bool) -> String {
    if !full_path {
        return name.to_string();
    }
    if parent_label.ends_with('/') {
        format!("{parent_label}{name}")
    } else {
        format!("{parent_label}/{name}")
    }
}

fn is_executable_mode(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn child_from_entry(entry: &fs::DirEntry) -> io::Result<Child> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let file_type = entry.file_type()?;
    let is_symlink = file_type.is_symlink();
    let is_dir = file_type.is_dir();
    let is_executable = if !is_dir && !is_symlink {
        entry.metadata().map(|metadata| is_executable_mode(&metadata)).unwrap_or(false)
    } else {
        false
    };
    Ok(Child { name, path: entry.path(), is_dir, is_symlink, is_executable })
}

fn list_children(path: &Path, options: &Options) -> io::Result<Vec<Child>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let child = match child_from_entry(&entry) {
            Ok(child) => child,
            Err(_) => continue,
        };
        if !options.all && child.name.starts_with('.') {
            continue;
        }
        if options.dirs_only && !child.is_dir {
            continue;
        }
        children.push(child);
    }
    children.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(children)
}

fn write_indent<W: Write>(
    writer: &mut W,
    continues: &[bool],
    is_last: bool,
    graphics: &LineGraphics,
) -> io::Result<()> {
    for &has_continuation in continues {
        let segment = if has_continuation { graphics.vertical } else { graphics.empty };
        writer.write_all(segment.as_bytes())?;
    }
    let joint = if is_last { graphics.last } else { graphics.branch };
    writer.write_all(joint.as_bytes())
}

fn write_summary<W: Write>(mut writer: W, summary: &Summary, dirs_only: bool) -> io::Result<()> {
    writeln!(writer)?;
    let directory_word = if summary.directories == 1 { "directory" } else { "directories" };
    if dirs_only {
        writeln!(writer, "{} {directory_word}", summary.directories)
    } else {
        let file_word = if summary.files == 1 { "file" } else { "files" };
        writeln!(writer, "{} {directory_word}, {} {file_word}", summary.directories, summary.files)
    }
}

fn write_entries<W: Write>(
    writer: &mut W,
    children: &[Child],
    parent_label: &str,
    options: &Options,
    graphics: &LineGraphics,
    continues: &mut Vec<bool>,
    level: u32,
) -> io::Result<WalkResult> {
    let mut result = WalkResult { summary: Summary::default(), had_error: false };
    for (index, child) in children.iter().enumerate() {
        let is_last = index + 1 == children.len();
        let label = child_label(parent_label, &child.name, options.full_path);
        let suffix = classify_suffix(child, options);
        let next_level = level.saturating_add(1);

        write_indent(writer, continues, is_last, graphics)?;
        if child.is_dir && can_descend(next_level, options.max_level) {
            match list_children(&child.path, options) {
                Ok(grandchildren) => {
                    writeln!(writer, "{label}{suffix}")?;
                    result.summary.directories = result.summary.directories.saturating_add(1);
                    continues.push(!is_last);
                    let nested = write_entries(
                        writer,
                        &grandchildren,
                        &label,
                        options,
                        graphics,
                        continues,
                        next_level,
                    )?;
                    continues.pop();
                    result.summary.add(nested.summary);
                    result.had_error |= nested.had_error;
                }
                Err(_) => {
                    writeln!(writer, "{label}{suffix}{ERROR_OPENING_DIR}")?;
                    result.summary.directories = result.summary.directories.saturating_add(1);
                    result.had_error = true;
                }
            }
        } else if child.is_dir {
            writeln!(writer, "{label}{suffix}")?;
            result.summary.directories = result.summary.directories.saturating_add(1);
        } else {
            writeln!(writer, "{label}{suffix}")?;
            result.summary.files = result.summary.files.saturating_add(1);
        }
    }
    Ok(result)
}

fn write_root<W: Write>(
    writer: &mut W,
    path: &Path,
    label: &str,
    options: &Options,
    graphics: &LineGraphics,
) -> io::Result<WalkResult> {
    // Follow a symlink given as the root, matching Linux `tree`.
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            writeln!(writer, "{label}{ERROR_OPENING_DIR}")?;
            return Ok(WalkResult { summary: Summary::default(), had_error: true });
        }
    };

    if !metadata.is_dir() {
        let suffix = if options.classify {
            if metadata.file_type().is_symlink() {
                "@"
            } else if is_executable_mode(&metadata) {
                "*"
            } else {
                ""
            }
        } else {
            ""
        };
        writeln!(writer, "{label}{suffix}")?;
        return Ok(WalkResult { summary: Summary { directories: 0, files: 1 }, had_error: false });
    }

    if !can_descend(0, options.max_level) {
        writeln!(writer, "{label}")?;
        return Ok(WalkResult { summary: Summary::default(), had_error: false });
    }

    match list_children(path, options) {
        Ok(children) => {
            writeln!(writer, "{label}")?;
            let mut continues = Vec::new();
            write_entries(writer, &children, label, options, graphics, &mut continues, 0)
        }
        Err(_) => {
            writeln!(writer, "{label}{ERROR_OPENING_DIR}")?;
            Ok(WalkResult { summary: Summary::default(), had_error: true })
        }
    }
}

/// Parses operands after `argv[0]`.
fn parse_args(args: &[String]) -> Result<(Options, Vec<&str>), String> {
    let mut options = Options::default();
    let mut operands = Vec::new();
    let mut options_ended = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            let flags: Vec<char> = arg.chars().skip(1).collect();
            if flags.is_empty() {
                return Err(format!("tree: invalid option -- '-'\n{USAGE}"));
            }
            let mut flag_index = 0;
            while flag_index < flags.len() {
                match flags[flag_index] {
                    'a' => options.all = true,
                    'd' => options.dirs_only = true,
                    'f' => options.full_path = true,
                    'F' => options.classify = true,
                    'S' => options.ascii = true,
                    'L' => {
                        let attached: String = flags[flag_index + 1..].iter().collect();
                        let value = if attached.is_empty() {
                            if index >= args.len() {
                                return Err(format!(
                                    "tree: Missing argument to -L option.\n{USAGE}"
                                ));
                            }
                            let next = args[index].clone();
                            index += 1;
                            next
                        } else {
                            attached
                        };
                        let level: u32 = value.parse().map_err(|_| {
                            format!("tree: Invalid level, must be greater than 0.\n{USAGE}")
                        })?;
                        if level == 0 {
                            return Err(format!(
                                "tree: Invalid level, must be greater than 0.\n{USAGE}"
                            ));
                        }
                        options.max_level = Some(level);
                        break;
                    }
                    flag => return Err(format!("tree: invalid option -- '{flag}'\n{USAGE}")),
                }
                flag_index += 1;
            }
            continue;
        }
        operands.push(arg.as_str());
    }
    Ok((options, operands))
}

fn run(args: &[String]) -> i32 {
    let (options, operands) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let paths: &[&str] = if operands.is_empty() { &[DEFAULT_ROOT] } else { operands.as_slice() };
    let graphics = graphics(&options);
    let mut stdout = io::stdout().lock();
    let mut summary = Summary::default();
    let mut status = 0;
    for path in paths {
        match write_root(&mut stdout, Path::new(path), path, &options, graphics) {
            Ok(walk) => {
                summary.add(walk.summary);
                if walk.had_error {
                    status = 1;
                }
            }
            Err(err) => {
                let _ = writeln!(io::stderr(), "tree: {err}");
                return 1;
            }
        }
    }
    if let Err(err) = write_summary(&mut stdout, &summary, options.dirs_only) {
        let _ = writeln!(io::stderr(), "tree: {err}");
        return 1;
    }
    status
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(name: &str) -> Self {
            let unique =
                SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("secular-tree-{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn join(&self, relative: &str) -> PathBuf {
            self.path.join(relative)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn render(path: &Path, label: &str, options: Options) -> String {
        let mut output = Vec::new();
        let walk = write_root(&mut output, path, label, &options, graphics(&options)).unwrap();
        write_summary(&mut output, &walk.summary, options.dirs_only).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        let (options, operands) = parse_args(&args).unwrap();
        assert_eq!(options, Options::default());
        assert!(operands.is_empty());
    }

    #[test]
    fn parse_args_clustered_flags_and_paths() {
        let args = ["-adS".to_string(), "src".to_string()];
        let (options, operands) = parse_args(&args).unwrap();
        assert!(options.all);
        assert!(options.dirs_only);
        assert!(options.ascii);
        assert_eq!(operands, ["src"]);
    }

    #[test]
    fn parse_args_level_attached_and_separate() {
        let attached = ["-L2".to_string()];
        assert_eq!(parse_args(&attached).unwrap().0.max_level, Some(2));
        let separate = ["-L".to_string(), "3".to_string(), "dir".to_string()];
        let (options, operands) = parse_args(&separate).unwrap();
        assert_eq!(options.max_level, Some(3));
        assert_eq!(operands, ["dir"]);
    }

    #[test]
    fn parse_args_rejects_zero_level() {
        let args = ["-L".to_string(), "0".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("Invalid level"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-z".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_double_dash_ends_options() {
        let args = ["--".to_string(), "-a".to_string()];
        assert_eq!(parse_args(&args).unwrap().1, ["-a"]);
    }

    #[test]
    fn write_indent_utf8_and_ascii() {
        let mut utf8 = Vec::new();
        write_indent(&mut utf8, &[true, false], true, &UTF8_GRAPHICS).unwrap();
        assert_eq!(String::from_utf8(utf8).unwrap(), "│       └── ");

        let mut ascii = Vec::new();
        write_indent(&mut ascii, &[true], false, &ASCII_GRAPHICS).unwrap();
        assert_eq!(String::from_utf8(ascii).unwrap(), "|   |-- ");
    }

    #[test]
    fn write_summary_pluralizes() {
        let mut one = Vec::new();
        write_summary(&mut one, &Summary { directories: 1, files: 1 }, false).unwrap();
        assert_eq!(String::from_utf8(one).unwrap(), "\n1 directory, 1 file\n");

        let mut many = Vec::new();
        write_summary(&mut many, &Summary { directories: 2, files: 0 }, true).unwrap();
        assert_eq!(String::from_utf8(many).unwrap(), "\n2 directories\n");
    }

    #[test]
    fn tree_lists_sorted_children_and_skips_hidden() {
        let tree = TempTree::new("basic");
        fs::create_dir(tree.join("a")).unwrap();
        fs::write(tree.join("a/x.txt"), b"x").unwrap();
        fs::write(tree.join("b.txt"), b"b").unwrap();
        fs::write(tree.join(".hidden"), b"h").unwrap();

        let options = Options { ascii: true, ..Options::default() };
        let output = render(&tree.path, ".", options);
        assert_eq!(
            output,
            ".\n\
             |-- a\n\
             |   `-- x.txt\n\
             `-- b.txt\n\
             \n\
             1 directory, 2 files\n"
        );
    }

    #[test]
    fn tree_all_includes_hidden_files() {
        let tree = TempTree::new("all");
        fs::write(tree.join(".hidden"), b"h").unwrap();
        fs::write(tree.join("visible"), b"v").unwrap();

        let options = Options { ascii: true, all: true, ..Options::default() };
        let output = render(&tree.path, ".", options);
        assert_eq!(
            output,
            ".\n\
             |-- .hidden\n\
             `-- visible\n\
             \n\
             0 directories, 2 files\n"
        );
    }

    #[test]
    fn tree_dirs_only_omits_files() {
        let tree = TempTree::new("dirs");
        fs::create_dir(tree.join("keep")).unwrap();
        fs::write(tree.join("skip.txt"), b"s").unwrap();

        let options = Options { ascii: true, dirs_only: true, ..Options::default() };
        let output = render(&tree.path, ".", options);
        assert_eq!(
            output,
            ".\n\
             `-- keep\n\
             \n\
             1 directory\n"
        );
    }

    #[test]
    fn tree_max_level_does_not_descend() {
        let tree = TempTree::new("level");
        fs::create_dir(tree.join("a")).unwrap();
        fs::write(tree.join("a/deep.txt"), b"d").unwrap();
        fs::write(tree.join("top.txt"), b"t").unwrap();

        let options = Options { ascii: true, max_level: Some(1), ..Options::default() };
        let output = render(&tree.path, ".", options);
        assert_eq!(
            output,
            ".\n\
             |-- a\n\
             `-- top.txt\n\
             \n\
             1 directory, 1 file\n"
        );
    }

    #[test]
    fn tree_classify_and_full_path() {
        let tree = TempTree::new("classify");
        fs::create_dir(tree.join("dir")).unwrap();
        fs::write(tree.join("file"), b"f").unwrap();

        let options =
            Options { ascii: true, classify: true, full_path: true, ..Options::default() };
        let output = render(&tree.path, ".", options);
        assert_eq!(
            output,
            ".\n\
             |-- ./dir/\n\
             `-- ./file\n\
             \n\
             1 directory, 1 file\n"
        );
    }

    #[test]
    fn tree_missing_root_is_an_error() {
        let mut output = Vec::new();
        let options = Options { ascii: true, ..Options::default() };
        let walk = write_root(
            &mut output,
            Path::new("/secular-tree-missing-path"),
            "missing",
            &options,
            graphics(&options),
        )
        .unwrap();
        assert!(walk.had_error);
        assert_eq!(walk.summary, Summary::default());
        assert_eq!(String::from_utf8(output).unwrap(), "missing [error opening dir]\n");
    }
}
