"""xa11y CLI — thin wrapper that delegates to the Rust implementation.

The Rust side owns both the error message and the exit code (see
``cli::run_main``), so this wrapper only forwards ``argv`` and propagates the
code. It deliberately does not catch and re-print exceptions: doing so used to
collapse every failure to exit 1 and double the "usage error: " prefix.
"""

import sys


def main() -> None:
    from xa11y._native import _cli_main

    try:
        code = _cli_main(sys.argv[1:])
    except KeyboardInterrupt:
        # Ctrl-C on a long-running subcommand (``events``, ``mcp``). 130 is the
        # conventional shell code for SIGINT.
        sys.exit(130)
    sys.exit(code)


if __name__ == "__main__":
    main()
