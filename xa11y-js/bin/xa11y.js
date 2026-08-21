#!/usr/bin/env node
// @ts-check
/**
 * The `xa11y` command for the Node package.
 *
 * A thin shim over the Rust CLI, exactly like the Python console script: the
 * same binary logic serves `cargo install xa11y`, `pip install xa11y`, and
 * this package, so the three cannot drift on commands, output, or exit codes.
 *
 * It loads `native.js` directly rather than `index.js`. The sugar layers there
 * (typed error subclasses, the EventEmitter subscription wrapper) exist for
 * library consumers and have nothing to add to a process that immediately
 * hands control to Rust.
 */

'use strict';

const { cliMain } = require('../native.js');

// `process.argv` is [node, script, ...args]; the Rust side expects the args
// alone, matching `std::env::args().skip(1)`.
const code = cliMain(process.argv.slice(2));

// `process.exitCode` rather than `process.exit(code)`: exit() truncates
// pending stdout writes on a pipe, which would cut off the last MCP response
// of a session. Setting the code lets Node drain and then exit with it.
process.exitCode = code;
