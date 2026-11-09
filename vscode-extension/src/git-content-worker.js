'use strict';

const { spawn } = require('node:child_process');
const { constants: bufferConstants } = require('node:buffer');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { BridgeError } = require('./bridge-client');

const MAX_REQUEST_BYTES = 1024 * 1024;
const MAX_METADATA_BYTES = 15 * 1024 * 1024;
const MAX_ERROR_BYTES = 64 * 1024;
const ROUTING_VARIABLES = new Set(['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_PREFIX',
  'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE']);

function resolveGitExecutable(executable, env) {
  if (path.isAbsolute(executable)) return executable;
  const hasDirectory = executable.includes('/') || (process.platform === 'win32' && executable.includes('\\'));
  const pathValue = env.PATH ?? Object.entries(env).find(([name]) => name.toUpperCase() === 'PATH')?.[1] ?? '';
  const directories = hasDirectory ? [''] : pathValue.split(path.delimiter);
  const extensions = process.platform === 'win32' && !path.extname(executable)
    ? (env.PATHEXT || '.COM;.EXE;.BAT;.CMD').split(';') : [''];
  for (const directory of directories) {
    for (const extension of extensions) {
      const candidate = path.resolve(directory, executable + extension);
      try {
        fs.accessSync(candidate, fs.constants.X_OK);
        if (fs.statSync(candidate).isFile()) return candidate;
      } catch { /* Continue searching PATH. */ }
    }
  }
  throw new BridgeError('unsupported_git', `VS Code's Git executable could not be resolved: ${executable}`);
}

function captureGitEnvironment(api) {
  const git = api?.git;
  if (!git || typeof git.path !== 'string' || !git.path ||
      !git.env || typeof git.env !== 'object' || Array.isArray(git.env)) {
    throw new BridgeError('unsupported_git', 'VS Code did not provide the Git executable and environment.');
  }
  const merged = { ...process.env, ...git.env }, env = {};
  for (const [name, value] of Object.entries(merged)) {
    if (ROUTING_VARIABLES.has(process.platform === 'win32' ? name.toUpperCase() : name) || value === undefined) continue;
    env[name] = String(value);
  }
  const executable = resolveGitExecutable(git.path, env);
  const entries = Object.entries(env).sort(([first], [second]) => first.localeCompare(second));
  const key = createHash('sha256').update(JSON.stringify([executable, entries])).digest('hex');
  return { executable, env, key };
}

class GitContentWorkers {
  constructor(options = {}) {
    this.spawn = options.spawn || spawn;
    this.killGroup = options.killGroup || ((pid, signal) => process.kill(-pid, signal));
    this.timeout = options.timeout ?? 120000;
    this.workers = new Set();
    this.disposed = false;
  }

  async run(executable, env, request, signal) {
    if (this.disposed) throw new BridgeError('disposed', 'Gitpane content service is unavailable.');
    if (signal?.aborted) throw new BridgeError('cancelled', 'Gitpane content request was cancelled.');
    const line = `${JSON.stringify(request)}\n`;
    if (Buffer.byteLength(line) > MAX_REQUEST_BYTES) throw new BridgeError('request_too_large', 'Git content request is too large.');
    const limit = ['metadata', 'working_metadata'].includes(request.operation.kind) ? MAX_METADATA_BYTES : bufferConstants.MAX_LENGTH;
    return await new Promise((resolve, reject) => {
      let child;
      try {
        child = this.spawn(executable, ['serve', '--content-worker'], {
          env, stdio: ['pipe', 'pipe', 'pipe'], detached: process.platform !== 'win32', windowsHide: true
        });
      } catch (error) {
        reject(new BridgeError('service_error', `Gitpane content service could not start: ${error.message}`));
        return;
      }
      let done = false, exited = false, failed, size = 0, errorSize = 0;
      const chunks = [], errors = [];
      const stop = error => {
        failed ||= error;
        child.stdin.destroy();
        if (!done && !exited && !killTimer) killTimer = setTimeout(() => {
          if (done || exited) return;
          try {
            if (process.platform !== 'win32' && child.pid) this.killGroup(child.pid, 'SIGKILL');
            else child.kill('SIGKILL');
          } catch { /* The worker may already have exited. */ }
        }, 1000);
      };
      let killTimer;
      const worker = { stop };
      this.workers.add(worker);
      const onAbort = () => stop(new BridgeError('cancelled', 'Gitpane content request was cancelled.'));
      signal?.addEventListener('abort', onAbort, { once: true });
      if (signal?.aborted) onAbort();
      const timer = setTimeout(() => stop(new BridgeError('timeout', 'Gitpane content service timed out.')), this.timeout);
      child.stdout.on('data', chunk => {
        size += chunk.length;
        if (size > limit) { stop(new BridgeError('response_too_large', 'Git content exceeds the supported size.')); return; }
        chunks.push(chunk);
      });
      child.stderr.on('data', chunk => {
        errorSize += chunk.length;
        if (errorSize > MAX_ERROR_BYTES) { stop(new BridgeError('protocol_error', 'Gitpane content service returned an oversized error.')); return; }
        errors.push(chunk);
      });
      for (const stream of [child.stdin, child.stdout, child.stderr]) {
        stream.on('error', error => stop(new BridgeError('service_error', `Gitpane content service failed: ${error.message}`)));
      }
      child.on('error', error => stop(new BridgeError('service_error', `Gitpane content service failed: ${error.message}`)));
      child.on('exit', () => { exited = true; clearTimeout(killTimer); });
      child.on('close', (code, exitSignal) => {
        done = true;
        clearTimeout(timer);
        clearTimeout(killTimer);
        signal?.removeEventListener('abort', onAbort);
        this.workers.delete(worker);
        if (failed) { reject(failed); return; }
        if (code !== 0) {
          let detail;
          try { detail = JSON.parse(Buffer.concat(errors).toString('utf8')); } catch { /* Invalid errors are reported as protocol failures. */ }
          if (detail && typeof detail.code === 'string' && typeof detail.message === 'string') {
            reject(new BridgeError(detail.code, detail.message));
          } else reject(new BridgeError('protocol_error', `Gitpane content service failed (code ${code}, signal ${exitSignal}).`));
          return;
        }
        try { resolve(Buffer.concat(chunks, size)); }
        catch { reject(new BridgeError('response_too_large', 'Git content exceeds the supported size.')); }
      });
      child.stdin.write(line, error => {
        if (error) stop(new BridgeError('service_error', `Gitpane content request failed: ${error.message}`));
      });
    });
  }

  dispose() {
    this.disposed = true;
    this.cancel();
  }

  cancel() {
    for (const worker of this.workers) worker.stop(new BridgeError('disposed', 'Gitpane content service was closed.'));
  }
}

module.exports = { GitContentWorkers, captureGitEnvironment };
