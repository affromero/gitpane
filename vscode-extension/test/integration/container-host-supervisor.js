'use strict';

// This process owns Docker create through cleanup. Its stdin is held open by
// the caller; EOF means the caller disappeared and cleanup must begin.
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

const config = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const label = 'gitpane.host-test';
const cidfile = path.join(config.root, 'container.id');
const resultPath = path.join(config.root, 'result.json');
const logPath = path.join(config.root, 'container.log');
const MAX_LOG = 1024 * 1024;
let aborted = false;
let abortReason;
let notifyAbort;
const abortedPromise = new Promise(resolve => { notifyAbort = resolve; });

function abort(reason) {
  if (aborted) return;
  aborted = true;
  abortReason = reason;
  notifyAbort();
}

function docker(args, timeoutMs = 15000, signal, captureBoth = false) {
  return new Promise((resolve, reject) => {
    let socket;
    try { socket = fs.statSync(config.socket); }
    catch (error) { return reject(new Error(`Docker socket is unavailable: ${error.message}`)); }
    if (!socket.isSocket() || socket.dev !== config.socketDevice || socket.ino !== config.socketInode) {
      return reject(new Error('Docker socket identity changed during the test.'));
    }
    const child = spawn(config.dockerPath, ['--host', `unix://${config.socket}`, ...args], {
      env: { PATH: process.env.PATH || '/usr/bin:/bin' }, stdio: ['ignore', 'pipe', 'pipe'], signal
    });
    let stdout = '';
    let stderr = '';
    let stdoutTruncated = false;
    let stderrTruncated = false;
    let settled = false;
    child.stdout.on('data', data => {
      const text = data.toString();
      if (text.length > MAX_LOG - stdout.length) stdoutTruncated = true;
      if (stdout.length < MAX_LOG) stdout += text.slice(0, MAX_LOG - stdout.length);
    });
    child.stderr.on('data', data => {
      const text = data.toString();
      if (text.length > MAX_LOG - stderr.length) stderrTruncated = true;
      if (stderr.length < MAX_LOG) stderr += text.slice(0, MAX_LOG - stderr.length);
    });
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill('SIGKILL');
      child.stdout.destroy();
      child.stderr.destroy();
      const error = new Error(`docker ${args[0]} timed out: ${stderr.trim()}${stderrTruncated ? '\n[stderr truncated]' : ''}`);
      error.timedOut = true;
      reject(error);
    }, timeoutMs);
    child.on('error', error => { if (!settled) { settled = true; clearTimeout(timer); reject(error); } });
    child.on('close', (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (code !== 0) return reject(new Error(`docker ${args[0]} failed (${code}, ${signal}): ${stderr.trim()}${stderrTruncated ? '\n[stderr truncated]' : ''}`));
      resolve(captureBoth ? { stdout, stderr, stdoutTruncated, stderrTruncated } : stdout.trim());
    });
  });
}

async function lookup() {
  const output = await docker(['ps', '-aq', '--no-trunc', '--filter', `label=${label}=${config.token}`]);
  const ids = output ? output.split(/\s+/) : [];
  if (ids.length > 1 || ids.some(id => !/^[a-f0-9]{64}$/.test(id))) throw new Error('Container identity is ambiguous.');
  return ids[0];
}

async function inspect(id) {
  if (!/^[a-f0-9]{64}$/.test(id || '')) throw new Error('Invalid container ID.');
  const value = JSON.parse(await docker(['inspect', '--format', '{{json .}}', id]));
  const mounts = new Map((value.Mounts || []).map(item => [item.Destination, item]));
  const wanted = [['/source', config.source, false], ['/extension', config.extension, false], ['/output', config.results, true]];
  if (value.Id !== id || value.Image !== config.image || value.Config?.Labels?.[label] !== config.token ||
      value.HostConfig?.NetworkMode !== 'none' || value.HostConfig?.PidMode !== '' ||
      value.HostConfig?.Init !== true || value.HostConfig?.Privileged || value.HostConfig?.ReadonlyRootfs !== true ||
      value.Config?.User !== `${config.uid}:${config.gid}` || value.HostConfig?.RestartPolicy?.Name !== 'no' ||
      !value.HostConfig?.CapDrop?.includes('ALL') || !value.HostConfig?.SecurityOpt?.includes('no-new-privileges') ||
      value.HostConfig?.PidsLimit !== 512 || value.HostConfig?.Memory !== 2 * 1024 * 1024 * 1024 ||
      value.HostConfig?.NanoCpus !== 2 * 1000 * 1000 * 1000 || mounts.size !== wanted.length ||
      wanted.some(([dest, source, writable]) => {
        const item = mounts.get(dest);
        return !item || item.Type !== 'bind' || item.Source !== source || item.RW !== writable;
      })) throw new Error('Container identity or isolation does not match the test run.');
  return value;
}

function outputBytes(root) {
  let bytes = 0;
  const visit = dir => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const target = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) visit(target);
      else if (entry.isFile()) bytes += fs.lstatSync(target).size;
      if (bytes > config.maxResultBytes) return;
    }
  };
  visit(root);
  return bytes;
}

async function cleanup(id) {
  const found = await lookup();
  if (!found) {
    if (id) {
      const exact = await docker(['ps', '-aq', '--no-trunc', '--filter', `id=${id}`]);
      if (exact) throw new Error('The recorded container exists without its expected test label.');
    }
    return { verified: true };
  }
  if (id && found !== id) throw new Error('A different container has the test label.');
  const value = await inspect(found);
  if (value.State?.Running) {
    try { await docker(['stop', '--time', '2', found], 10000); } catch {
      const still = await inspect(found);
      if (still.State?.Running) await docker(['kill', found], 10000);
    }
  }
  if ((await inspect(found)).State?.Running) throw new Error('Container is still running after cleanup.');
  let diagnosticError;
  try {
    const logs = await docker(['logs', found], 15000, undefined, true);
    fs.writeFileSync(logPath, `[stdout]\n${logs.stdout}${logs.stdoutTruncated ? '\n[stdout truncated]' : ''}` +
      `\n[stderr]\n${logs.stderr}${logs.stderrTruncated ? '\n[stderr truncated]' : ''}`);
  }
  catch (error) { diagnosticError = `Could not save container logs: ${error.message}`; }
  await docker(['rm', found]);
  if (await lookup()) throw new Error('Container still exists after removal.');
  return { verified: true, diagnosticError };
}

async function run() {
  let id;
  let exitCode;
  let error;
  let cleanupVerified = false;
  let createUncertain = false;
  let monitor;
  try {
    if (aborted) throw new Error(abortReason);
    const actual = await docker(['image', 'inspect', '--format', '{{.Id}}', config.image]);
    if (actual !== config.image) throw new Error('The pre-staged image does not match its immutable ID.');
    if (aborted) throw new Error(abortReason);
    const mounts = [
      `type=bind,src=${config.source},dst=/source,readonly`,
      `type=bind,src=${config.extension},dst=/extension,readonly`,
      `type=bind,src=${config.results},dst=/output`
    ];
    const args = ['create', '--pull=never', '--cidfile', cidfile, '--label', `${label}=${config.token}`,
      '--init', '--network=none', '--restart=no', '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges',
      '--pids-limit=512', '--memory=2g', '--cpus=2', '--user', `${config.uid}:${config.gid}`,
      '--tmpfs', '/tmp:rw,nosuid,size=64m,mode=1777', '-e', 'TMPDIR=/output', '-e', 'GITPANE_TEST_EXTENSION=/extension',
      '-e', 'XDG_CONFIG_HOME=/output/config', '-e', 'XDG_CACHE_HOME=/output/cache',
      '-e', 'XDG_DATA_HOME=/output/data', '-e', 'XDG_RUNTIME_DIR=/output/runtime',
      ...mounts.flatMap(mount => ['--mount', mount]), '--entrypoint', '/usr/bin/timeout', config.image,
      '--signal=TERM', `--kill-after=${config.graceSeconds}s`, `${config.timeoutSeconds}s`, ...config.command];
    let created;
    try { created = await docker(args, 30000); }
    catch (failure) { createUncertain = true; throw failure; }
    id = created.trim();
    if (!/^[a-f0-9]{64}$/.test(id) || fs.readFileSync(cidfile, 'utf8').trim() !== id) {
      createUncertain = true;
      throw new Error('Docker did not publish a matching container ID.');
    }
    await inspect(id);
    if (aborted) throw new Error(abortReason);
    await docker(['start', id]);
    if (aborted) throw new Error(abortReason);
    monitor = setInterval(() => {
      try { if (outputBytes(config.results) > config.maxResultBytes) abort('Test output exceeded the monitored size limit.'); }
      catch (failure) { abort(`Cannot monitor test output: ${failure.message}`); }
    }, 500);
    const waiting = new AbortController();
    abortedPromise.then(() => waiting.abort());
    const waited = docker(['wait', id], (config.timeoutSeconds + 30) * 1000, waiting.signal);
    const finished = await Promise.race([waited.then(value => ({ value })), abortedPromise.then(() => ({ aborted: true }))]);
    if (finished.aborted) throw new Error(abortReason);
    if (outputBytes(config.results) > config.maxResultBytes) throw new Error('Test output exceeded the monitored size limit.');
    exitCode = Number(finished.value);
    if (!Number.isInteger(exitCode)) throw new Error('Docker returned an invalid exit status.');
    if (exitCode !== 0) throw new Error(`Container test exited with status ${exitCode}.`);
  } catch (failure) { error = failure?.message || String(failure); }
  finally {
    if (monitor) clearInterval(monitor);
    try {
      const outcome = await cleanup(id);
      cleanupVerified = outcome.verified && !createUncertain;
      if (outcome.diagnosticError) error = `${error ? `${error}\n` : ''}${outcome.diagnosticError}`;
    } catch (failure) { error = `${error ? `${error}\n` : ''}Cleanup could not be verified: ${failure.message}`; }
    const result = { success: !error && cleanupVerified && exitCode === 0, exitCode, error, cleanupVerified, label: config.token };
    fs.writeFileSync(`${resultPath}.tmp`, JSON.stringify(result, null, 2));
    fs.renameSync(`${resultPath}.tmp`, resultPath);
    process.stdin.destroy();
  }
}

let started = false;
process.stdin.setEncoding('utf8');
process.stdin.on('data', data => {
  if (started || !data.includes('start\n')) return;
  started = true;
  run().catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; });
});
process.stdin.on('end', () => {
  abort('The test runner disconnected.');
  if (!started) { started = true; run().catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; }); }
});
for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => abort(`The supervisor received ${signal}.`));
process.stdout.write('READY\n');
