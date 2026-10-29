'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { codeExecutable, runHost } = require('./host');
const { isolatedGitEnv } = require('./git-env');

const sourceExtension = path.resolve(__dirname, '../..');
const extension = process.env.GITPANE_TEST_EXTENSION || sourceExtension;
const code = codeExecutable();
const count = Number(process.env.GITPANE_SCALE_COUNT || '100');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-vscode-scale-'));
const root = path.join(temporary, 'repos');
let safeToRemove = false;

(async () => {
try {
  if (!Number.isSafeInteger(count) || count < 1 || count > 10000) throw new Error('GITPANE_SCALE_COUNT must be between 1 and 10000.');
  if (!path.isAbsolute(extension) || !fs.existsSync(path.join(extension, 'package.json'))) throw new Error(`Invalid extension directory: ${extension}`);
  fs.mkdirSync(root);
  const gitEnv = isolatedGitEnv();
  for (let index = 0; index < count; index++) {
    const repo = path.join(root, `repo-${String(index).padStart(5, '0')}`);
    fs.mkdirSync(repo);
    const init = spawnSync('git', ['init', '-q'], { cwd: repo, env: gitEnv, encoding: 'utf8' });
    if (init.error || init.status !== 0) throw new Error(`git init failed: ${init.error?.message || init.stderr}`);
    fs.writeFileSync(path.join(repo, 'new.txt'), `${index}\n`);
  }
  const env = {
    ...isolatedGitEnv(),
    GITPANE_TEST_EXTENSION: extension,
    GITPANE_SCALE_COUNT: String(count),
    GITPANE_TEST_RESULT: path.join(temporary, 'result.json')
  };
  const args = [
    '--new-window', '--wait', '--disable-workspace-trust',
    '--user-data-dir', path.join(temporary, 'profile'),
    '--extensions-dir', path.join(temporary, 'extensions'),
    `--extensionDevelopmentPath=${extension}`,
    `--extensionTestsPath=${path.join(__dirname, 'scale-suite.js')}`,
    root
  ];
  const host = await runHost(code, args, env, 300000);
  if (!fs.existsSync(env.GITPANE_TEST_RESULT)) throw new Error('VS Code did not write scale test results.');
  safeToRemove = true;
  const result = JSON.parse(fs.readFileSync(env.GITPANE_TEST_RESULT, 'utf8'));
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (result.error) {
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
