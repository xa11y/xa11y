// Expand test paths ourselves: npm uses cmd.exe on Windows, where single
// quotes are literal. Passing that quoted glob to Node succeeds with 0 tests.
// Explicit paths also work on Node 18, which predates test-runner glob support.
import { readdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const packageRoot = fileURLToPath(new URL('../', import.meta.url));
const integration = process.argv[2] === 'integ';
const directory = resolve(packageRoot, integration ? '../tests/suites/js' : '__test__/unit');

function testFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return testFiles(path);
    return entry.isFile() && entry.name.endsWith('.test.js') ? [path] : [];
  }).sort();
}

const files = testFiles(directory);
if (files.length === 0) throw new Error(`No test files found in ${directory}`);
const args = ['--test', ...(integration ? ['--test-timeout=60000'] : []), ...files];
const result = spawnSync(process.execPath, args, { cwd: packageRoot, stdio: 'inherit' });
if (result.error) throw result.error;
process.exit(result.status ?? 1);
