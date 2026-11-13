'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { codeExecutable, runHost } = require('./host');
const { isolatedGitEnv } = require('./git-env');

const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-vscode-reload-'));
const repo = path.join(temporary, 'workspace');
const extension = path.resolve(__dirname, '../..');
let complete = false;

function git(...args) {
  const result = spawnSync('git', ['-C', repo, '-c', 'user.name=Gitpane Test',
    '-c', 'user.email=gitpane-test@example.invalid', ...args], { env: isolatedGitEnv(), encoding: 'utf8' });
  if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr);
}

(async () => {
  try {
    fs.mkdirSync(repo);
    git('init', '-q');
    for (const name of ['staged', 'working']) fs.writeFileSync(path.join(repo, `${name}.txt`), `original ${name}\n`);
    git('add', '.');
    git('commit', '-qm', 'fixture');
    fs.writeFileSync(path.join(repo, 'staged.txt'), 'staged change\n');
    git('add', 'staged.txt');
    fs.writeFileSync(path.join(repo, 'working.txt'), 'working change\n');
    const config = { repo, extension, result: path.join(temporary, 'result.json'), phase: path.join(temporary, 'phase.json'),
      backend: process.env.GITPANE_TEST_BINARY || '', remote: null };
    fs.writeFileSync(path.join(repo, '.gitpane-host-test.json'), JSON.stringify(config));
    const env = { ...isolatedGitEnv(), GITPANE_TEST_EXTENSION: extension, GITPANE_TEST_REPO: repo,
      GITPANE_TEST_RESULT: config.result };
    fs.mkdirSync(path.join(temporary, 'profile', 'User'), { recursive: true });
    fs.writeFileSync(path.join(temporary, 'profile', 'User', 'settings.json'), JSON.stringify({
      'update.mode': 'none', 'extensions.autoUpdate': false, 'git.autofetch': false
    }));
    const args = ['--new-window', '--wait', '--disable-workspace-trust',
      '--user-data-dir', path.join(temporary, 'profile'), '--extensions-dir', path.join(temporary, 'extensions'),
      '--shared-data-dir', path.join(temporary, 'shared-data'), '--use-inmemory-secretstorage',
      `--extensionDevelopmentPath=${extension}`, `--extensionDevelopmentPath=${path.join(__dirname, 'reload-driver')}`, repo];
    const host = await runHost(codeExecutable(), args, env, 180000);
    const results = JSON.parse(fs.readFileSync(env.GITPANE_TEST_RESULT, 'utf8'));
    for (const result of results) {
      process.stdout.write(`${result.ok ? 'PASS' : 'FAIL'} ${result.name}\n`);
      if (!result.ok) process.stderr.write(`${result.error}\n`);
    }
    if (results.some(result => !result.ok)) throw new Error(`VS Code reload failed.\n${host.output}`);
    complete = true;
  } catch (error) {
    console.error(error.stack || String(error));
    process.exitCode = 1;
  } finally {
    if (complete) fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    else console.error(`Preserved test directory for diagnosis: ${temporary}`);
  }
})();
