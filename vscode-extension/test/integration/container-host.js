'use strict';

// Run the optional VS Code host suite only inside a separately supervised
// Linux container. The supervisor owns the Docker lifecycle; this process can
// disappear without leaving a running test indefinitely.
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const supervisor = path.join(__dirname, 'container-host-supervisor.js');
const IMAGE_ID = /^sha256:[a-f0-9]{64}$/;
const MAX_INPUT_BYTES = 256 * 1024 * 1024;
const MAX_INPUT_ENTRIES = 10000;

function absoluteDirectory(value, name) {
  if (typeof value !== 'string' || !path.isAbsolute(value)) throw new Error(`${name} must be an absolute directory.`);
  const real = fs.realpathSync(value);
  if (!fs.statSync(real).isDirectory() || /[,\n\r]/.test(real)) {
    throw new Error(`${name} must be a directory without commas or newlines in its path.`);
  }
  return real;
}

function snapshot(source, target, root = source, budget = { bytes: 0, entries: 0 }, depth = 0) {
  if (depth > 32) throw new Error('Test input nesting exceeds the staging limit.');
  fs.mkdirSync(target, { recursive: true, mode: 0o755 });
  fs.chmodSync(target, 0o755);
  for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
    if (++budget.entries > MAX_INPUT_ENTRIES) throw new Error('Test inputs contain too many entries.');
    const from = path.join(source, entry.name);
    const to = path.join(target, entry.name);
    const stat = fs.lstatSync(from);
    if (stat.isSymbolicLink() || !contains(root, fs.realpathSync(from))) throw new Error(`Unsafe test input: ${from}`);
    if (stat.isDirectory()) snapshot(from, to, root, budget, depth + 1);
    else if (stat.isFile()) {
      budget.bytes += stat.size;
      if (!Number.isSafeInteger(budget.bytes) || budget.bytes > MAX_INPUT_BYTES) throw new Error('Test inputs exceed the staging size limit.');
      const input = fs.openSync(from, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
      let output;
      try {
        const opened = fs.fstatSync(input);
        if (!opened.isFile() || opened.dev !== stat.dev || opened.ino !== stat.ino || opened.size !== stat.size) {
          throw new Error(`Test input changed during staging: ${from}`);
        }
        output = fs.openSync(to, 'wx', 0o600);
        const buffer = Buffer.allocUnsafe(256 * 1024);
        let remaining = stat.size;
        while (remaining > 0) {
          const read = fs.readSync(input, buffer, 0, Math.min(buffer.length, remaining), null);
          if (!read) throw new Error(`Test input changed during staging: ${from}`);
          let offset = 0;
          while (offset < read) {
            const written = fs.writeSync(output, buffer, offset, read - offset);
            if (!written) throw new Error(`Could not stage test input: ${from}`);
            offset += written;
          }
          remaining -= read;
        }
        if (fs.readSync(input, buffer, 0, 1, null)) throw new Error(`Test input changed during staging: ${from}`);
      } finally {
        if (output !== undefined) fs.closeSync(output);
        fs.closeSync(input);
      }
      fs.chmodSync(to, stat.mode & 0o111 ? 0o555 : 0o444);
    } else throw new Error(`Unsupported test input: ${from}`);
  }
}

function digestTree(root) {
  const hash = crypto.createHash('sha256');
  let bytes = 0;
  let entries = 0;
  const visit = (directory, relative, depth) => {
    if (depth > 32) throw new Error('Test input nesting exceeds the staging limit.');
    for (const entry of fs.readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      if (++entries > MAX_INPUT_ENTRIES) throw new Error('Test inputs contain too many entries.');
      const full = path.join(directory, entry.name);
      const name = path.join(relative, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`Test inputs cannot contain symlinks: ${full}`);
      hash.update(`${entry.isDirectory() ? 'd' : 'f'}\0${name}\0`);
      if (entry.isDirectory()) visit(full, name, depth + 1);
      else if (entry.isFile()) {
        const stat = fs.lstatSync(full);
        if (!stat.isFile()) throw new Error(`Test input changed during staging: ${full}`);
        bytes += stat.size;
        if (!Number.isSafeInteger(bytes) || bytes > MAX_INPUT_BYTES) throw new Error('Test inputs exceed the staging size limit.');
        hash.update(stat.mode & 0o111 ? 'x' : '-');
        hash.update(`${stat.size}\0`);
        const fd = fs.openSync(full, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
        try {
          const buffer = Buffer.allocUnsafe(256 * 1024);
          let remaining = stat.size;
          while (remaining > 0) {
            const read = fs.readSync(fd, buffer, 0, Math.min(buffer.length, remaining), null);
            if (!read) throw new Error(`Test input changed during staging: ${full}`);
            hash.update(buffer.subarray(0, read));
            remaining -= read;
          }
          if (fs.readSync(fd, buffer, 0, 1, null)) throw new Error(`Test input changed during staging: ${full}`);
        } finally { fs.closeSync(fd); }
      } else throw new Error(`Unsupported test input: ${full}`);
    }
  };
  visit(root, '', 0);
  return hash.digest('hex');
}

function contains(parent, child) {
  const relative = path.relative(parent, child);
  return relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

function validate(options) {
  if (!options || !IMAGE_ID.test(options.image || '')) throw new Error('Use a locally staged immutable image ID (sha256:...).');
  if (typeof process.getuid !== 'function' || typeof process.getgid !== 'function') throw new Error('Container host tests require a Unix Docker client.');
  const socket = fs.realpathSync(options.socket || '/var/run/docker.sock');
  if (!fs.statSync(socket).isSocket()) throw new Error('Docker must use a local Unix socket.');
  const source = absoluteDirectory(options.source, 'Source');
  const extension = absoluteDirectory(options.extension, 'Extension');
  if (!Array.isArray(options.command) || !options.command.length || !options.command[0] ||
      options.command.some(item => typeof item !== 'string' || item.includes('\0'))) {
    throw new Error('A fixed container command is required.');
  }
  if (!Number.isSafeInteger(options.timeoutSeconds) || options.timeoutSeconds < 1 || options.timeoutSeconds > 3600) {
    throw new Error('Container timeout must be between 1 and 3600 seconds.');
  }
  if (options.graceSeconds !== undefined && (!Number.isSafeInteger(options.graceSeconds) || options.graceSeconds < 1 || options.graceSeconds > 30)) {
    throw new Error('Container stop grace must be between 1 and 30 seconds.');
  }
  if (options.maxResultBytes !== undefined && (!Number.isSafeInteger(options.maxResultBytes) || options.maxResultBytes < 1)) {
    throw new Error('The monitored output limit must be a positive integer.');
  }
  if (options.dockerPath !== undefined && (typeof options.dockerPath !== 'string' || !path.isAbsolute(options.dockerPath))) {
    throw new Error('Docker executable override must be an absolute path.');
  }
  return { socket, source, extension };
}

function startContainer(options) {
  const paths = validate(options);
  const tests = absoluteDirectory(path.join(paths.source, 'vscode-extension', 'test', 'integration'), 'Integration tests');
  const temporary = fs.realpathSync(os.tmpdir());
  if (contains(paths.extension, temporary) || contains(tests, temporary)) {
    throw new Error('The temporary directory cannot be inside the extension or integration-test inputs.');
  }
  const beforeTests = digestTree(tests);
  const beforeExtension = digestTree(paths.extension);
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-container-host-'));
  const results = path.join(root, 'results');
  const source = path.join(root, 'source');
  const extension = path.join(root, 'extension');
  fs.mkdirSync(results, { mode: 0o700 });
  for (const name of ['config', 'cache', 'data', 'runtime', 'home']) fs.mkdirSync(path.join(results, name), { mode: 0o700 });
  const uid = process.getuid() === 0 ? 65534 : process.getuid();
  const gid = process.getuid() === 0 ? 65534 : process.getgid();
  if (process.getuid() === 0) {
    for (const directory of [results, ...['config', 'cache', 'data', 'runtime', 'home'].map(name => path.join(results, name))]) {
      fs.chownSync(directory, uid, gid);
    }
  }
  const stagedTests = path.join(source, 'vscode-extension', 'test', 'integration');
  try {
    snapshot(tests, stagedTests);
    snapshot(paths.extension, extension);
    if (beforeTests !== digestTree(tests) || beforeTests !== digestTree(stagedTests) ||
        beforeExtension !== digestTree(paths.extension) || beforeExtension !== digestTree(extension)) {
      throw new Error('Test inputs changed during staging.');
    }
  } catch (error) {
    throw new Error(`${error.message}; staging artifacts: ${root}`);
  }
  const config = {
    image: options.image, socket: paths.socket, source: fs.realpathSync(source),
    extension: fs.realpathSync(extension), results: fs.realpathSync(results), root: fs.realpathSync(root),
    token: crypto.randomBytes(24).toString('hex'), command: options.command,
    timeoutSeconds: options.timeoutSeconds, maxResultBytes: options.maxResultBytes || 512 * 1024 * 1024,
    graceSeconds: options.graceSeconds || 5, uid, gid,
    dockerPath: options.dockerPath || 'docker'
  };
  const socketStat = fs.statSync(config.socket);
  config.socketDevice = socketStat.dev;
  config.socketInode = socketStat.ino;
  for (const value of [config.source, config.extension, config.results]) {
    if (/[,\n\r]/.test(value)) throw new Error(`Cannot mount path containing a comma or newline: ${value}`);
  }
  const configPath = path.join(root, 'config.json');
  fs.writeFileSync(configPath, JSON.stringify(config), { mode: 0o600, flag: 'wx' });
  const child = spawn(process.execPath, [supervisor, configPath], {
    detached: true, stdio: ['pipe', 'pipe', 'pipe'], env: { PATH: process.env.PATH || '/usr/bin:/bin' }
  });
  let output = '';
  let errors = '';
  let ready = false;
  const readyPromise = new Promise((resolve, reject) => {
    const deadline = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`Container supervisor did not become ready; artifacts: ${root}`)); }, 10000);
    child.on('error', error => { clearTimeout(deadline); reject(error); });
    child.stdout.on('data', chunk => {
      output += chunk.toString();
      if (output.length > 1024 * 1024) output = output.slice(-1024 * 1024);
      if (!ready && output.includes('READY\n')) { ready = true; clearTimeout(deadline); resolve(); }
    });
    child.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-1024 * 1024); });
    child.on('close', (code, signal) => {
      clearTimeout(deadline);
      if (!ready) reject(new Error(`Container supervisor did not start (${code}, ${signal}): ${errors}`));
    });
  });
  const completion = new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('close', (code, signal) => {
      try {
        const result = JSON.parse(fs.readFileSync(path.join(root, 'result.json'), 'utf8'));
        resolve({ ...result, root, supervisorCode: code, signal });
      } catch (error) { reject(new Error(`Container supervisor did not publish a result (${code}, ${signal}): ${errors}; ${error.message}`)); }
    });
  });
  completion.catch(() => {});
  return { root, child, ready: readyPromise, completion, start: () => child.stdin.write('start\n'), cancel: () => child.stdin.end() };
}

async function main(args) {
  if (args.length !== 4 || args[0] !== '--image' || args[2] !== '--extension') {
    throw new Error('Usage: node container-host.js --image sha256:<id> --extension <extracted Linux VSIX directory>');
  }
  const suite = process.env.GITPANE_CONTAINER_SUITE || 'functional';
  if (!['functional', 'scale'].includes(suite)) throw new Error('GITPANE_CONTAINER_SUITE must be functional or scale.');
  const source = path.resolve(__dirname, '../../..');
  const command = ['xvfb-run', '-a', 'node', `/source/vscode-extension/test/integration/${suite === 'scale' ? 'scale.js' : 'run.js'}`];
  const run = startContainer({ image: args[1], extension: args[3], source, command, timeoutSeconds: suite === 'scale' ? 360 : 240 });
  process.stdout.write(`Test artifacts: ${run.root}\n`);
  await run.ready;
  run.start();
  const result = await run.completion;
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (!result.success) process.exitCode = 1;
}

if (require.main === module) main(process.argv.slice(2)).catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; });

module.exports = { startContainer };
