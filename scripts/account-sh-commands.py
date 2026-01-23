#!/usr/bin/env python3
"""
Script to count commands used in shell files and their total use count.
Filters commands by those available in /usr/bin and /usr/sbin.
Output format: count cmd <example args in one of the invocation>
"""

import sys
import os
from collections import defaultdict, Counter
import shlex


def is_known_command(cmd):
    """Check if command exists in /usr/bin or /usr/sbin"""
    return (os.path.isfile(f"/usr/bin/{cmd}") or
            os.path.isfile(f"/usr/sbin/{cmd}"))


def parse_shell_line(line):
    """
    Very simple shell line parsing to extract commands.
    Returns list of (command, full_invocation) tuples found in the line.
    """
    commands = []

    # Skip empty lines and comments
    line = line.strip()
    if not line or line.startswith('#'):
        return commands

    # Simple approach: look for command patterns in the line
    # Split on common command separators
    parts = line.replace(' && ', ' ; ').replace(' || ', ' ; ').replace(' | ', ' ; ').split(' ; ')

    for part in parts:
        part = part.strip()
        if not part:
            continue

        # Skip if statements and other control structures
        if part.startswith(('if ', 'then', 'else', 'elif ', 'fi', 'for ', 'while ', 'until ', 'case ')):
            continue

        # Try to find command at the beginning
        try:
            tokens = shlex.split(part)
            if tokens:
                cmd = tokens[0]
                # Skip shell builtins and keywords
                if cmd in ['if', 'then', 'else', 'elif', 'fi', 'for', 'do', 'done',
                          'while', 'until', 'case', 'esac', 'function', 'export',
                          'local', 'declare', 'eval', 'exec', 'source', '.', ':',
                          'true', 'false', 'return', 'exit', 'break', 'continue',
                          'shift', 'wait', 'trap', 'set', 'unset', 'readonly',
                          '[', 'test']:
                    continue

                # Check if it's a known command
                if is_known_command(cmd):
                    # Remove the command name from the beginning for cleaner output
                    example = part
                    if example.startswith(cmd):
                        example = example[len(cmd):].lstrip()
                    commands.append((cmd, example))
        except ValueError:
            continue

    return commands


def main():
    if len(sys.argv) < 2:
        print("Usage: python3 account-sh-commands.py <SHELL_FILES>", file=sys.stderr)
        sys.exit(1)

    shell_files = sys.argv[1:]

    # Dictionary to store command -> (count, example_invocation)
    command_stats = {}

    for filepath in shell_files:
        try:
            with open(filepath, 'r', encoding='utf-8', errors='ignore') as f:
                for line_num, line in enumerate(f, 1):
                    commands = parse_shell_line(line)
                    for cmd, invocation in commands:
                        if cmd not in command_stats:
                            command_stats[cmd] = [0, invocation]
                        command_stats[cmd][0] += 1

        except FileNotFoundError:
            print(f"Warning: File not found: {filepath}", file=sys.stderr)
        except PermissionError:
            print(f"Warning: Permission denied: {filepath}", file=sys.stderr)
        except Exception as e:
            print(f"Warning: Error reading {filepath}: {e}", file=sys.stderr)

    # Sort by count descending, then by command name
    sorted_commands = sorted(command_stats.items(),
                           key=lambda x: (-x[1][0], x[0]))

    # Output results
    for cmd, (count, example) in sorted_commands:
        print(f"{count:3d} {cmd} {example}")


if __name__ == "__main__":
    main()