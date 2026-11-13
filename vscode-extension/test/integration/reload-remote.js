'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { codeExecutable, runHost } = require('./host');
const { isolatedGitEnv } = require('./git-env');

const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-ssh-test-'));
const shared = path.join(temporary, 'shared');
const keys = path.join(temporary, 'server-keys');
const workspace = path.join(shared, 'workspace');
const extension = path.join(shared, 'extension');
const suffix = randomUUID().slice(0, 12);
const image = `gitpane-ssh-test:${suffix}`;
let container, imageBuilt = false, completed = false;

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', timeout: 600000, ...options });
  if (result.error || result.status !== 0) throw new Error(`${command} failed: ${result.error?.message || result.stderr || result.stdout}`);
  return result.stdout.trim();
}

function git(...args) {
  return run('git', ['-C', workspace, '-c', 'user.name=Gitpane Test',
    '-c', 'user.email=gitpane-test@example.invalid', ...args], { env: isolatedGitEnv() });
}

function makeWritable(directory) {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) { fs.chmodSync(target, 0o777); makeWritable(target); }
    else if (entry.isFile()) fs.chmodSync(target, 0o666);
  }
}

(async () => {
  try {
    const binary = process.env.GITPANE_LINUX_TEST_BINARY;
    if (!binary || !path.isAbsolute(binary) || !fs.existsSync(binary)) throw new Error('Set GITPANE_LINUX_TEST_BINARY to the Linux Gitpane backend.');
    fs.mkdirSync(shared, { mode: 0o777 });
    fs.chmodSync(shared, 0o777);
    fs.mkdirSync(keys, { mode: 0o755 });
    fs.mkdirSync(workspace, { mode: 0o777 });
    fs.chmodSync(workspace, 0o777);
    fs.cpSync(path.resolve(__dirname, '../..'), extension, { recursive: true });
    fs.copyFileSync(binary, path.join(shared, 'gitpane'));
    fs.chmodSync(path.join(shared, 'gitpane'), 0o755);
    git('init', '-q');
    for (const name of ['staged', 'working']) fs.writeFileSync(path.join(workspace, `${name}.txt`), `original ${name}\n`);
    git('add', '.');
    git('commit', '-qm', 'fixture');
    fs.writeFileSync(path.join(workspace, 'staged.txt'), 'staged change\n');
    git('add', 'staged.txt');
    fs.writeFileSync(path.join(workspace, 'working.txt'), 'working change\n');
    fs.writeFileSync(path.join(workspace, '.gitpane-host-test.json'), JSON.stringify({ repo: '/test/workspace',
      extension: '/test/extension', result: '/test/result.json', phase: '/test/phase.json',
      backend: '/test/gitpane', remote: 'ssh-remote' }));
    makeWritable(workspace);
    run('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', path.join(temporary, 'client_key')]);
    run('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', path.join(keys, 'host_key')]);
    fs.copyFileSync(path.join(temporary, 'client_key.pub'), path.join(keys, 'authorized_keys'));
    fs.writeFileSync(path.join(keys, 'sshd_config'), [
      'Port 22', 'ListenAddress 0.0.0.0', 'HostKey /keys/host_key', 'PidFile /run/sshd.pid',
      'AuthorizedKeysFile /keys/authorized_keys', 'StrictModes no', 'PasswordAuthentication no',
      'KbdInteractiveAuthentication no', 'PermitRootLogin no', 'AllowUsers gitpanetest', 'UsePAM no'
    ].join('\n') + '\n');
    run('docker', ['build', '-q', '-t', image, path.join(__dirname, 'remote')]);
    imageBuilt = true;
    container = run('docker', ['run', '-d', '--label', `gitpane.test=${suffix}`, '-p', '127.0.0.1::22',
      '--mount', `type=bind,src=${shared},dst=/test`, '--mount', `type=bind,src=${keys},dst=/keys,readonly`, image]);
    run('docker', ['exec', '-u', 'gitpanetest', container, 'git', 'config', '--global', '--add', 'safe.directory', '/test/workspace']);
    const port = run('docker', ['port', container, '22/tcp']).match(/:(\d+)$/)?.[1];
    if (!port) throw new Error('The isolated SSH server did not publish a port.');
    const sshConfig = path.join(temporary, 'ssh_config');
    const knownHosts = path.join(temporary, 'known_hosts');
    fs.writeFileSync(knownHosts, `[127.0.0.1]:${port} ${fs.readFileSync(path.join(keys, 'host_key.pub'), 'utf8')}`);
    fs.writeFileSync(sshConfig, ['Host gitpane-test', '  HostName 127.0.0.1', `  Port ${port}`, '  User gitpanetest',
      `  IdentityFile ${path.join(temporary, 'client_key')}`, '  IdentitiesOnly yes', '  StrictHostKeyChecking yes',
      `  UserKnownHostsFile ${knownHosts}`].join('\n') + '\n');
    run('ssh', ['-F', sshConfig, '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15', 'gitpane-test', 'true']);
    const profile = path.join(temporary, 'profile'), extensions = path.join(temporary, 'extensions');
    fs.mkdirSync(path.join(profile, 'User'), { recursive: true });
    fs.writeFileSync(path.join(profile, 'User', 'settings.json'), JSON.stringify({ 'remote.SSH.configFile': sshConfig,
      'remote.SSH.useLocalServer': false, 'remote.SSH.remotePlatform': { 'gitpane-test': 'linux' },
      'security.workspace.trust.enabled': false, 'extensions.autoUpdate': false, 'update.mode': 'none' }));
    const existing = process.env.GITPANE_REMOTE_SSH_EXTENSION;
    if (existing) {
      fs.mkdirSync(extensions);
      fs.cpSync(existing, path.join(extensions, 'ms-vscode-remote.remote-ssh'), { recursive: true });
    } else {
      run(codeExecutable(), ['--install-extension', 'ms-vscode-remote.remote-ssh@0.128.0', '--extensions-dir', extensions,
        '--user-data-dir', profile, '--shared-data-dir', path.join(temporary, 'shared-data'), '--force']);
    }
    const remote = 'vscode-remote://ssh-remote+gitpane-test';
    const env = { ...isolatedGitEnv(), GITPANE_TEST_RESULT: path.join(shared, 'result.json') };
    const args = ['--new-window', '--wait', '--disable-workspace-trust', '--user-data-dir', profile, '--extensions-dir', extensions,
      '--shared-data-dir', path.join(temporary, 'shared-data'), '--use-inmemory-secretstorage',
      `--extensionDevelopmentPath=${remote}/test/extension`,
      `--extensionDevelopmentPath=${remote}/test/extension/test/integration/reload-driver`,
      '--folder-uri', `${remote}/test/workspace`];
    const host = await runHost(codeExecutable(), args, env, 420000);
    const results = JSON.parse(fs.readFileSync(env.GITPANE_TEST_RESULT, 'utf8'));
    for (const result of results) {
      process.stdout.write(`${result.ok ? 'PASS' : 'FAIL'} Remote SSH: ${result.name}\n`);
      if (!result.ok) process.stderr.write(`${result.error}\n`);
    }
    if (results.some(result => !result.ok)) throw new Error(`Remote SSH reload failed.\n${host.output}`);
    completed = true;
  } catch (error) {
    console.error(error.stack || String(error));
    process.exitCode = 1;
  } finally {
    try {
      if (container) {
        const label = run('docker', ['inspect', '-f', '{{index .Config.Labels "gitpane.test"}}', container]);
        if (label !== suffix) throw new Error(`Container ${container} has an unexpected identity.`);
        run('docker', ['rm', '-f', container]);
      }
      if (imageBuilt) run('docker', ['image', 'rm', image]);
    } catch (error) { console.error(`Remote test cleanup failed: ${error.message}`); completed = false; process.exitCode = 1; }
    if (completed) fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    else console.error(`Preserved test directory for diagnosis: ${temporary}`);
  }
})();
