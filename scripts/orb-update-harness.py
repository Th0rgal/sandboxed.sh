#!/usr/bin/python3
"""Narrow sudo entry point for an exact, officially distributed fleet harness."""
import os
import re
import sys


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in {'claude', 'codex', 'opencode', 'grok'} or not re.fullmatch(r'\d+\.\d+\.\d+', sys.argv[2]):
        raise SystemExit('Usage: orb-update-harness {claude|codex|opencode|grok} MAJOR.MINOR.PATCH')
    if os.geteuid() != 0:
        raise SystemExit('Administrator permission required')
    os.execve('/usr/bin/python3', ['python3', '/usr/local/sbin/update-node-harnesses', '--only', sys.argv[1], '--version', sys.argv[2]], {'PATH': '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})


if __name__ == '__main__':
    main()
