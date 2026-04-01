"""xa11y CLI — thin wrapper that delegates to the Rust implementation."""

import sys


def main() -> None:
    try:
        from xa11y._native import _cli_main

        _cli_main(sys.argv[1:])
    except KeyboardInterrupt:
        pass
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
