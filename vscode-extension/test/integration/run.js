'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { codeExecutable, runHost } = require('./host');

const sourceExtension = path.resolve(__dirname, '../..');
const extension = process.env.GITPANE_TEST_EXTENSION || sourceExtension;
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-vscode-test-'));
const root = path.join(temporary, 'workspace with spaces');
const repo = path.join(root, 'repo-a');
const nested = path.join(root, 'projects', 'repo-b');
const secondRoot = path.join(temporary, 'second workspace');
const second = path.join(secondRoot, 'repo-c');
const outside = path.join(temporary, 'outside repo');
const added = path.join(temporary, 'added workspace');
const workspaceFile = path.join(temporary, 'multi-root.code-workspace');
let safeToRemove = false;

function git(cwd, ...args) {
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: os.devNull };
  for (const key of ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_CONFIG_COUNT']) delete env[key];
  const result = spawnSync('git', ['-c', 'user.name=Gitpane Test', '-c', 'user.email=gitpane-test@example.invalid', ...args], {
    cwd, env, encoding: 'utf8'
  });
  if (result.error || result.status !== 0) {
    throw new Error(`git ${args[0]} failed: ${result.error?.message || result.stderr}`);
  }
}

function write(name, contents) {
  fs.writeFileSync(path.join(repo, name), contents);
}

(async () => {
try {
  if (!path.isAbsolute(extension) || !fs.existsSync(path.join(extension, 'package.json'))) {
    throw new Error(`Set GITPANE_TEST_EXTENSION to an absolute extension directory: ${extension}`);
  }
  for (const folder of [repo, nested, second, outside, added]) {
    fs.mkdirSync(folder, { recursive: true });
    git(folder, 'init', '-q');
  }
  for (const name of ['staged', 'working', 'partial', 'original', 'deleted']) write(`${name}.txt`, `original ${name}\n`);
  git(repo, 'add', '--', '.');
  git(repo, 'commit', '-qm', 'fixture');
  write('staged.txt', 'staged value\n');
  write('working.txt', 'working value\n');
  write('partial.txt', 'index partial\n');
  git(repo, 'add', '--', 'staged.txt', 'partial.txt');
  write('partial.txt', 'working partial\n');
  git(repo, 'mv', '--', 'original.txt', 'renamed.txt');
  fs.unlinkSync(path.join(repo, 'deleted.txt'));
  write('untracked.txt', 'untracked value\n');
  fs.writeFileSync(path.join(nested, 'nested.txt'), 'nested change\n');
  fs.writeFileSync(path.join(second, 'second.txt'), 'second change\n');
  fs.writeFileSync(path.join(outside, 'outside.txt'), 'outside change\n');
  fs.writeFileSync(path.join(added, 'added.txt'), 'added change\n');
  fs.writeFileSync(workspaceFile, JSON.stringify({ folders: [{ path: root }, { path: secondRoot }], settings: { 'git.repositoryScanMaxDepth': 4 } }));

  const env = {
    ...process.env,
    GITPANE_TEST_EXTENSION: extension,
    GITPANE_TEST_REPO: fs.realpathSync(repo),
    GITPANE_TEST_NESTED: fs.realpathSync(nested),
    GITPANE_TEST_SECOND: fs.realpathSync(second),
    GITPANE_TEST_OUTSIDE: fs.realpathSync(outside),
    GITPANE_TEST_ADDED: fs.realpathSync(added),
    GITPANE_TEST_RESULT: path.join(temporary, 'result.json')
  };
  const args = [
    '--new-window', '--wait', '--disable-workspace-trust',
    '--user-data-dir', path.join(temporary, 'profile'),
    '--extensions-dir', path.join(temporary, 'extensions'),
    `--extensionDevelopmentPath=${extension}`,
    `--extensionTestsPath=${path.join(__dirname, 'suite.js')}`,
    workspaceFile
  ];
  const host = await runHost(codeExecutable(), args, env, 180000);
  const tests = JSON.parse(fs.readFileSync(env.GITPANE_TEST_RESULT, 'utf8'));
  safeToRemove = true;
  for (const entry of tests) {
    process.stdout.write(`${entry.ok ? 'PASS' : 'FAIL'} ${entry.name}\n`);
    if (!entry.ok) process.stderr.write(`${entry.error}\n`);
  }
  if (tests.some(entry => !entry.ok)) {
    process.stderr.write(host.output);
    process.exitCode = 1;
  }
} catch (error) {
  console.error(error.stack || String(error));
  process.exitCode = 1;
} finally {
  if (safeToRemove) {
    fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  } else {
    console.error(`Preserved test directory for diagnosis: ${temporary}`);
  }
}
})();
