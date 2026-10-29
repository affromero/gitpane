'use strict';

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { BridgeError } = require('./bridge-client');
const { visibleRepository } = require('./model');

const MAX_LINE_BYTES = 1024 * 1024;
const OPERATIONS = new Set(['stage', 'unstage', 'discard', 'delete_untracked']);

function normalizeAction(method, params) {
  if (!params || typeof params.repo !== 'string' || !path.isAbsolute(params.repo)) {
    throw new BridgeError('invalid_request', 'A repository is required.');
  }
  if (method === 'session_goto' && ['tmux', 'herdr'].includes(params.backend) &&
    typeof params.session === 'string' && params.session.length > 0) {
    return { repo: params.repo, backend: params.backend, session: params.session };
  }
  if (method === 'repo_sync' && ['push', 'pull', 'pull_rebase'].includes(params.operation) &&
    (params.expected_branch === null || typeof params.expected_branch === 'string') &&
    typeof params.expected_head === 'string' && /^[a-f0-9]{40,64}$/i.test(params.expected_head)) {
    return { repo: params.repo, operation: params.operation,
      expected_branch: params.expected_branch, expected_head: params.expected_head };
  }
  if (method === 'submodule_action' && ['sync', 'update', 'pull_latest', 'pull_with_submodules'].includes(params.operation) &&
    (params.expected_branch === null || typeof params.expected_branch === 'string') &&
    typeof params.expected_head === 'string' && /^[a-f0-9]{40,64}$/i.test(params.expected_head)) {
    return { repo: params.repo, operation: params.operation,
      expected_branch: params.expected_branch, expected_head: params.expected_head };
  }
  if (typeof params.path !== 'string') {
    throw new BridgeError('invalid_request', 'A target path is required.');
  }
  if (method === 'file_action' && OPERATIONS.has(params.operation)) {
    const destructive = ['discard', 'delete_untracked'].includes(params.operation);
    if (destructive && (typeof params.expected_token !== 'string' || !/^[a-f0-9]{64}$/.test(params.expected_token))) {
      throw new BridgeError('invalid_request', 'Destructive file actions require a current confirmation.');
    }
    return { repo: params.repo, path: params.path, operation: params.operation,
      ...(destructive ? { expected_token: params.expected_token } : {}) };
  }
  if (method === 'worktree_create' && path.isAbsolute(params.path) && typeof params.branch === 'string') {
    return { repo: params.repo, path: params.path, branch: params.branch };
  }
  if (method === 'worktree_remove' && path.isAbsolute(params.path) &&
    /^[a-f0-9]{64}$/.test(params.expected_repo_identity || '') &&
    /^[a-f0-9]{64}$/.test(params.expected_worktree_identity || '')) {
    return { repo: params.repo, path: params.path, expected_repo_identity: params.expected_repo_identity,
      expected_worktree_identity: params.expected_worktree_identity };
  }
  throw new BridgeError('invalid_request', 'Unsupported Gitpane mutation.');
}

class MutationWorker {
  constructor(executable, options) {
    this.onStderr = options.onStderr;
    this.dispatched = false;
    this.finished = false;
    this.pending = undefined;
    this.parts = [];
    this.bytes = 0;
    this.child = options.spawn(executable, ['serve', '--mutation-once'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    this.closed = new Promise(resolve => { this.resolveClosed = resolve; });
    this.child.stdout.on('data', chunk => this.receive(chunk));
    this.child.stdout.on('error', error => this.failTransport(error));
    this.child.stdin.on('error', error => this.failTransport(error));
    this.child.stderr.on('data', chunk => this.onStderr(chunk.toString()));
    this.child.stderr.on('error', error => this.failTransport(error));
    this.child.on('error', error => this.failTransport(error));
    this.child.on('close', (code, signal) => {
      this.finished = true;
      if (this.pending) this.failTransport(new Error(`Service exited (code ${code}, signal ${signal}).`));
      this.resolveClosed();
    });
  }

  failTransport(error) {
    if (!this.pending) return;
    const pending = this.pending;
    this.pending = undefined;
    clearTimeout(pending.timer);
    const code = this.dispatched ? 'outcome_unknown' : 'service_error';
    const prefix = this.dispatched ? 'Gitpane action outcome is unknown' : 'Gitpane action could not start';
    pending.reject(new BridgeError(code, `${prefix}: ${error.message}`));
  }

  receive(chunk) {
    const data = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    let start = 0;
    while (start < data.length) {
      const newline = data.indexOf(10, start);
      const end = newline < 0 ? data.length : newline;
      const segment = data.subarray(start, end);
      this.bytes += segment.length;
      if (this.bytes > MAX_LINE_BYTES) {
        this.failTransport(new Error('Service response exceeds the size limit.'));
        return;
      }
      this.parts.push(segment);
      if (newline < 0) return;
      let response;
      try {
        const line = new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(this.parts, this.bytes));
        response = JSON.parse(line);
      } catch {
        this.failTransport(new Error('Service returned invalid JSON or UTF-8.'));
        return;
      }
      this.parts = [];
      this.bytes = 0;
      if (!this.pending || !response || response.id !== this.pending.id ||
        (Object.hasOwn(response, 'result') === Object.hasOwn(response, 'error'))) {
        this.failTransport(new Error('Service returned an invalid response.'));
        return;
      }
      const pending = this.pending;
      this.pending = undefined;
      clearTimeout(pending.timer);
      if (Object.hasOwn(response, 'error')) {
        if (!response.error || typeof response.error.code !== 'string' || typeof response.error.message !== 'string') {
          pending.reject(new BridgeError(this.dispatched ? 'outcome_unknown' : 'protocol_error', 'Gitpane service returned an invalid error.'));
        } else {
          pending.reject(new BridgeError(response.error.code, response.error.message));
        }
      } else pending.resolve(response.result);
      start = newline + 1;
    }
  }

  call(id, method, params, timeoutMs) {
    if (this.finished) return Promise.reject(new BridgeError(this.dispatched ? 'outcome_unknown' : 'service_error', 'Gitpane action service is not running.'));
    const line = `${JSON.stringify({ id, method, params })}\n`;
    if (Buffer.byteLength(line) > MAX_LINE_BYTES) return Promise.reject(new BridgeError('request_too_large', 'Gitpane request exceeds the size limit.'));
    return new Promise((resolve, reject) => {
      const timer = timeoutMs > 0 ? setTimeout(() => {
        this.failTransport(new Error('Service initialization timed out.'));
        this.cancelBeforeSend();
      }, timeoutMs) : undefined;
      this.pending = { id, resolve, reject, timer };
      try {
        if (id === 2) {
          this.dispatched = true;
          this.child.stdin.end(line);
        } else this.child.stdin.write(line);
      } catch (error) { this.failTransport(error); }
    });
  }

  cancelBeforeSend() {
    if (!this.dispatched && !this.finished) this.child.kill();
  }
}

class MutationManager {
  constructor(options) {
    this.getSnapshot = options.getSnapshot;
    this.isCurrent = options.isCurrent;
    this.realpath = options.realpath || fs.realpath;
    this.spawn = options.spawn || spawn;
    this.onStderr = options.onStderr || (() => {});
    this.handshakeTimeoutMs = options.handshakeTimeoutMs ?? 30000;
    this.locks = new Map();
    this.workers = new Set();
    this.disposed = false;
  }

  async execute(method, params, options = {}) {
    if (this.disposed) throw new BridgeError('disposed', 'Gitpane actions are unavailable.');
    const action = normalizeAction(method, params);
    if (typeof options.lockKey !== 'string' || !path.isAbsolute(options.lockKey) || typeof options.validateTarget !== 'function') {
      throw new BridgeError('invalid_request', 'A trusted Git common directory and current target are required.');
    }
    const context = await this.getSnapshot();
    if (!context || !path.isAbsolute(context.executable) || !Array.isArray(context.roots) ||
      context.roots.some(root => typeof root !== 'string' || !path.isAbsolute(root)) ||
      !Array.isArray(context.rootSources) || context.rootSources.length !== context.roots.length ||
      context.rootSources.some(source => typeof source !== 'string' || !path.isAbsolute(source)) ||
      !Array.isArray(context.rootIdentities) || context.rootIdentities.length !== context.roots.length ||
      context.rootIdentities.some(identity => typeof identity !== 'string' || !/^[a-f0-9]{64}$/.test(identity))) {
      throw new BridgeError('invalid_workspace', 'Gitpane workspace scope is unavailable.');
    }
    const snapshot = { executable: context.executable, roots: [...context.roots], rootSources: [...context.rootSources], rootIdentities: [...context.rootIdentities], generation: context.generation };
    const repo = await this.realpath(action.repo);
    let lock;
    try {
      lock = await this.realpath(options.lockKey);
      if (!(await fs.stat(lock)).isDirectory()) throw new Error('Not a directory');
    } catch {
      throw new BridgeError('invalid_request', 'The Git common directory is unavailable or invalid.');
    }
    const roots = await Promise.all(snapshot.roots.map(root => this.realpath(root)));
    if (!visibleRepository(repo, roots, value => value)) {
      throw new BridgeError('outside_workspace', 'Repository is outside the current workspace.');
    }
    if (this.disposed || !await this.isCurrent(snapshot, repo)) {
      throw new BridgeError('workspace_changed', 'Workspace changed before the Gitpane action could start.');
    }
    if (this.locks.has(lock)) throw new BridgeError('busy', 'A Gitpane action is already running for this repository.');
    const token = {};
    this.locks.set(lock, token);
    let worker;
    try {
      worker = new MutationWorker(snapshot.executable, {
        spawn: this.spawn, onStderr: this.onStderr
      });
      this.workers.add(worker);
      let initialized;
      try {
        initialized = await worker.call(1, 'initialize', { version: 1, workspace_roots: roots,
          workspace_root_sources: snapshot.rootSources, workspace_root_identities: snapshot.rootIdentities }, this.handshakeTimeoutMs);
      } catch (error) {
        if (error?.code === 'invalid_request' && /unknown field.*workspace_root_(?:identities|sources)/i.test(error.message)) {
          throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support workspace identity checks. Update the backend and try again.');
        }
        throw error;
      }
      if (!initialized || initialized.version !== 1) throw new BridgeError('unsupported_version', 'Gitpane service returned an unsupported protocol version.');
      const currentRepo = await this.realpath(action.repo);
      if (this.disposed || currentRepo !== repo || !await this.isCurrent(snapshot, repo) ||
        !await options.validateTarget() || this.disposed || !await this.isCurrent(snapshot, repo)) {
        throw new BridgeError('workspace_changed', 'Workspace or selected target changed before the Gitpane action could start.');
      }
      return await worker.call(2, method, { ...action, repo }, 0);
    } finally {
      if (!worker) {
        if (this.locks.get(lock) === token) this.locks.delete(lock);
      } else {
        worker.cancelBeforeSend();
        worker.closed.then(() => {
          this.workers.delete(worker);
          if (this.locks.get(lock) === token) this.locks.delete(lock);
        });
      }
    }
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    for (const worker of this.workers) worker.cancelBeforeSend();
  }
}

module.exports = { MutationManager };
