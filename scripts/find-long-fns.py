#!/usr/bin/env python3
"""
Find Rust functions with more than 50 lines.
Usage: scripts/find-long-fns.py [path]
If path is given, runs git grep in that directory.
Otherwise runs in current directory.
"""

import subprocess
import sys
import os
import re

def run_git_grep(path="."):
    """Run git grep -w 'fn' and return output lines."""
    cmd = ["git", "grep", "-n", "-w", "fn"]
    try:
        result = subprocess.run(cmd, cwd=path, capture_output=True, text=True, check=True)
        return result.stdout.splitlines()
    except subprocess.CalledProcessError as e:
        # git grep returns non-zero when no matches found
        if e.returncode == 1:
            return []
        raise

def extract_function_name(signature):
    """Extract function name from a function signature line.

    Examples:
    - "pub fn parse_options(matches: &clap::ArgMatches) -> Result<AddGroupCmd> {"
    - "fn base64_encode(input: &[u8]) -> String {"
    - "async fn fetch_data(url: &str) -> Result<()> {"
    Returns function name or None if cannot parse.
    """
    # Remove leading visibility modifiers and async
    # Match patterns like: pub fn, pub(crate) fn, async fn, fn
    pattern = r'^(?:pub(?:\([^)]+\))?\s+)?(?:async\s+)?fn\s+(\w+)'
    match = re.search(pattern, signature)
    if match:
        return match.group(1)
    return None

def find_closing_brace_simple(lines, start_idx, start_line_num, debug=False):
    """Find the line number of the matching closing brace for a function using indent-based '}' matching.

    Args:
        lines: list of all lines in the file
        start_idx: index in lines where function starts (0-based)
        start_line_num: actual line number of function start (1-based)
        debug: if True, print debugging information

    Returns:
        tuple (end_line_num, end_idx) or (None, None) if not found
    """
    # Compute indentation of the function definition line
    fn_line = lines[start_idx]
    indent_len = len(fn_line) - len(fn_line.lstrip())
    indent_str = fn_line[:indent_len]
    if debug:
        print(f"  Function indent: '{indent_str}' ({indent_len} chars)")

    for i in range(start_idx, len(lines)):
        line = lines[i]
        # For zero indent, require line starts with '}' (no leading whitespace)
        if indent_len == 0:
            if line.startswith('}'):
                end_line_num = start_line_num + (i - start_idx)
                if debug:
                    print(f"  Found closing brace at line {end_line_num}: {line.rstrip()}")
                return end_line_num, i
        else:
            # Check if line starts with indent_str and next non-space character is '}'
            if line.startswith(indent_str):
                rest = line[indent_len:]
                if rest.lstrip().startswith('}'):
                    end_line_num = start_line_num + (i - start_idx)
                    if debug:
                        print(f"  Found closing brace at line {end_line_num}: {line.rstrip()}")
                    return end_line_num, i
    return None, None

def find_closing_brace(lines, start_idx, start_line_num, debug=False):
    """Find the line number of the matching closing brace for a function.

    Args:
        lines: list of all lines in the file
        start_idx: index in lines where function starts (0-based)
        start_line_num: actual line number of function start (1-based)
        debug: if True, print debugging information

    Returns:
        tuple (end_line_num, end_idx) or (None, None) if not found
    """
    brace_count = 0
    in_string = False
    in_char = False
    escape_next = False
    # For raw strings: track delimiter length (0 for regular string)
    raw_delimiter_len = 0
    # For block comments: track nesting depth
    comment_depth = 0

    for i in range(start_idx, len(lines)):
        line = lines[i]
        j = 0
        while j < len(line):
            ch = line[j]

            if debug and ch in '{}' and not in_string and not in_char and comment_depth == 0:
                print(f"  Line {start_line_num + (i - start_idx)} col {j}: '{ch}' brace_count {brace_count} -> {brace_count + (1 if ch == '{' else -1)}")

            # Handle escape sequences (only for non-raw strings)
            if escape_next:
                escape_next = False
                j += 1
                continue

            # Handle comments (only when not in string/char)
            if not in_string and not in_char:
                if ch == '/' and j + 1 < len(line):
                    next_ch = line[j + 1]
                    if next_ch == '/':
                        # Line comment, skip rest of line
                        break
                    elif next_ch == '*':
                        comment_depth += 1
                        if debug:
                            print(f"  Line {start_line_num + (i - start_idx)} col {j}: start block comment depth {comment_depth}")
                        j += 2
                        continue
                if comment_depth > 0 and ch == '*' and j + 1 < len(line) and line[j + 1] == '/':
                    comment_depth -= 1
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: end block comment depth {comment_depth}")
                    j += 2
                    continue
                if comment_depth > 0:
                    j += 1
                    continue

            # Handle strings and chars
            if not in_string and not in_char and comment_depth == 0:
                # Check for raw string prefix: r#*" where # can be 0 or more #
                # Also handle byte strings b" and br#"
                # We'll look ahead for patterns
                if ch == 'r' and j + 1 < len(line) and line[j + 1] == '"':
                    # r" regular raw string (no #)
                    raw_delimiter_len = 0
                    in_string = True
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: start raw string r\"")
                    j += 2
                    continue
                elif ch == 'r' and j + 2 < len(line) and line[j + 1] == '#' and line[j + 2] == '"':
                    # r#" delimiter length 1
                    raw_delimiter_len = 1
                    in_string = True
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: start raw string r#\"")
                    j += 3
                    continue
                elif ch == 'r' and j + 3 < len(line) and line[j + 1] == '#' and line[j + 2] == '#' and line[j + 3] == '"':
                    # r##" delimiter length 2
                    raw_delimiter_len = 2
                    in_string = True
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: start raw string r##\"")
                    j += 4
                    continue
                # Similarly for b" and br#" but we treat same as regular strings
                # For simplicity, just treat any '"' as start of string
                pass

            # Escape sequences only for non-raw strings and char literals
            if (in_string and raw_delimiter_len == 0) or in_char:
                if ch == '\\':
                    escape_next = True
                    j += 1
                    continue

            if not in_char and comment_depth == 0 and ch == '"':
                if in_string:
                    # Check if raw string delimiter matches
                    if raw_delimiter_len == 0:
                        # Regular string ends
                        in_string = False
                        if debug:
                            print(f"  Line {start_line_num + (i - start_idx)} col {j}: end regular string")
                    else:
                        # Check for closing delimiter: "#* where # matches raw_delimiter_len
                        k = 1
                        while k <= raw_delimiter_len and j + k < len(line) and line[j + k] == '#':
                            k += 1
                        if k-1 == raw_delimiter_len:
                            # Found matching delimiter
                            in_string = False
                            raw_delimiter_len = 0
                            if debug:
                                print(f"  Line {start_line_num + (i - start_idx)} col {j}: end raw string with {k-1}#")
                            j += k
                            continue
                else:
                    # Starting a regular string
                    in_string = True
                    raw_delimiter_len = 0
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: start regular string")
                j += 1
                continue

            if not in_string and comment_depth == 0 and ch == '\'':
                # Check if this is a lifetime (like 'static, 'a) or a character literal
                if j + 1 < len(line) and (line[j + 1].isalpha() or line[j + 1] == '_'):
                    # Lifetime: skip the identifier
                    j += 1  # skip the quote
                    # skip identifier characters (letters, digits, underscores)
                    while j < len(line) and (line[j].isalnum() or line[j] == '_'):
                        j += 1
                    continue
                else:
                    # Character literal
                    in_char = not in_char
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: {'start' if in_char else 'end'} char literal")
                    j += 1
                    continue

            # Count braces (only when not in string/char/comment)
            if not in_string and not in_char and comment_depth == 0:
                if ch == '{':
                    brace_count += 1
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: {{ brace_count={brace_count}")
                elif ch == '}':
                    brace_count -= 1
                    if debug:
                        print(f"  Line {start_line_num + (i - start_idx)} col {j}: }} brace_count={brace_count}")
                    if brace_count == 0:
                        # Found matching closing brace
                        end_line_num = start_line_num + (i - start_idx)
                        if debug:
                            print(f"  Found matching closing brace at line {end_line_num}, idx {i}")
                        return end_line_num, i

            j += 1

    if debug:
        print(f"  No matching closing brace found, brace_count={brace_count}")
    return None, None

def process_file(filepath, function_lines):
    """Process a single file to find function line counts.

    Args:
        filepath: path to Rust source file
        function_lines: list of (line_num, signature) for function starts in this file

    Returns:
        list of (func_name, start_line, end_line, line_count)
    """
    try:
        with open(filepath, 'r', encoding='utf-8', errors='ignore') as f:
            lines = f.readlines()
    except FileNotFoundError:
        return []

    results = []
    # Sort by line number to process in order
    function_lines.sort(key=lambda x: x[0])

    for line_num, signature in function_lines:
        func_name = extract_function_name(signature)
        if not func_name:
            continue

        start_idx = line_num - 1  # Convert to 0-based index
        if start_idx >= len(lines):
            continue

        debug = False
        if debug:
            print(f"\nDEBUG: Function {func_name} at line {line_num}")
            print(f"  Signature: {signature}")
            print(f"  Start idx: {start_idx}")

        end_line_num, end_idx = find_closing_brace_simple(lines, start_idx, line_num, debug=debug)
        if end_line_num is None:
            if debug:
                print(f"  ERROR: No closing brace found")
            continue

        line_count = end_line_num - line_num + 1
        if debug:
            print(f"  End line: {end_line_num}, line count: {line_count}")
        if line_count > 50:
            results.append((func_name, line_num, end_line_num, line_count))

    return results

def main():
    if len(sys.argv) > 1:
        path = sys.argv[1]
        if not os.path.isdir(path):
            print(f"Error: {path} is not a directory")
            sys.exit(1)
    else:
        path = "."

    # Run git grep
    grep_lines = run_git_grep(path)
    if not grep_lines:
        print("No functions found.")
        return

    # Parse git grep output: "filename:line:signature"
    # Group by filename
    file_functions = {}
    for line in grep_lines:
        # Split into filename, line_num, and rest (signature)
        # git grep -n outputs "filename:line:content"
        first_colon = line.find(':')
        if first_colon == -1:
            continue
        second_colon = line.find(':', first_colon + 1)
        if second_colon == -1:
            continue

        filename = line[:first_colon]
        try:
            line_num = int(line[first_colon + 1:second_colon])
        except ValueError:
            continue

        signature = line[second_colon + 1:].strip()

        # Only consider lines that contain 'fn' (should be true due to grep -w)
        if 'fn' not in signature:
            continue

        if filename not in file_functions:
            file_functions[filename] = []
        file_functions[filename].append((line_num, signature))

    # Process each file
    all_results = []
    for filename, funcs in file_functions.items():
        filepath = os.path.join(path, filename) if path != "." else filename
        results = process_file(filepath, funcs)
        for func_name, start_line, end_line, line_count in results:
            all_results.append((filename, func_name, line_count))

    # Sort by line count descending
    all_results.sort(key=lambda x: x[2], reverse=True)

    # Output results
    for filename, func_name, line_count in all_results:
        print(f"{filename} {func_name}(): {line_count} lines")

if __name__ == "__main__":
    main()
