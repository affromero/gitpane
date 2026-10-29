'use strict';

const path = require('node:path');
const { spawn } = require('node:child_process');

const MAX_REQUEST_BYTES = 1024 * 1024;
const MAX_RESPONSE_BYTES = 16 * 1024 * 1024;
const MAX_PENDING = 64;

class BridgeError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'BridgeError';
    this.code = code;
  }
}

class BridgeClient {
  constructor(executable, options = {}) {
    if (!path.isAbsolute(executable)) throw new Error('Bridge executable must be an absolute path.');
    this.executable = executable;
    this.spawn = options.spawn || spawn;
    this.timeoutMs = options.timeoutMs ?? 30000;
    this.serviceArgs = options.serviceArgs || ['serve'];
    this.killGraceMs = options.killGraceMs ?? 1000;
    this.maxResponseBytes = options.maxResponseBytes ?? MAX_RESPONSE_BYTES;
    this.onStderr = options.onStderr || (() => {});
    this.onFailure = options.onFailure || (() => {});
    this.child = undefined;
    this.pending = new Map();
    this.expired = new Set();
    this.nextId = 1;
    this.parts = [];
    this.lineBytes = 0;
    this.initialization = undefined;
    this.disposed = false;
  }

  start() {
    if (this.disposed) throw new BridgeError('disposed', 'Gitpane service has been disposed.');
    if (this.child) return;
    const child = this.spawn(this.executable, this.serviceArgs, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    this.child = child;
    this.lastChild = child;
    this.closed = new Promise(resolve => child.once('close', resolve));
    child.stdout.on('data', chunk => this.receive(child, chunk));
    child.stdout.on('error', error => this.abort(child, new BridgeError('io_error', `Gitpane service output failed: ${error.message}`)));
    child.stdin.on('error', error => this.abort(child, new BridgeError('io_error', `Gitpane service input failed: ${error.message}`)));
    child.stderr.on('data', chunk => this.onStderr(chunk.toString()));
    child.stderr.on('error', error => this.abort(child, new BridgeError('io_error', `Gitpane service diagnostics failed: ${error.message}`)));
    child.on('error', error => this.abort(child, new BridgeError('spawn_error', `Could not start Gitpane service: ${error.message}`)));
    child.on('close', (code, signal) => {
      this.abort(child, new BridgeError('service_exited', `Gitpane service exited (code ${code}, signal ${signal}).`));
    });
  }

  initialize(workspaceRoots, identities, sources) {
    if (!Array.isArray(workspaceRoots) || workspaceRoots.some(root => typeof root !== 'string' || !path.isAbsolute(root))) {
      return Promise.reject(new BridgeError('invalid_workspace', 'Workspace roots must be absolute paths.'));
    }
    try { this.start(); }
    catch (error) { return Promise.reject(error); }
    const child = this.child;
    if (identities !== undefined && (!Array.isArray(identities) || identities.length !== workspaceRoots.length ||
      identities.some(token => typeof token !== 'string' || !/^[a-f0-9]{64}$/.test(token)))) {
      return Promise.reject(new BridgeError('invalid_workspace', 'Workspace identities are unavailable.'));
    }
    if (sources !== undefined && (!Array.isArray(sources) || sources.length !== workspaceRoots.length ||
      sources.some(source => typeof source !== 'string' || !path.isAbsolute(source)))) {
      return Promise.reject(new BridgeError('invalid_workspace', 'Workspace folder sources are unavailable.'));
    }
    const initialization = this.call('initialize', { version: 1, workspace_roots: [...workspaceRoots],
      ...(identities ? { workspace_root_identities: [...identities] } : {}),
      ...(sources ? { workspace_root_sources: [...sources] } : {}) }).catch(error => {
      if ((identities || sources) && error?.code === 'invalid_request' && /unknown field.*workspace_root_(?:identities|sources)/i.test(error.message)) {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support workspace identity checks. Update the backend and try again.');
      }
      throw error;
    }).then(result => {
      if (!result || result.version !== 1) {
        const error = new BridgeError('unsupported_version', 'Gitpane service returned an unsupported protocol version.');
        this.abort(child, error);
        throw error;
      }
      return result;
    });
    this.initialization = initialization;
    return initialization;
  }

  async workspaceRootIdentities() {
    try { return await this.request('workspace_root_identities', {}); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support workspace identity checks. Update the backend and try again.');
      }
      throw error;
    }
  }

  async sessionLiveness(repos, backend = 'auto') {
    try { return await this.request('session_liveness', { repos, backend }); }
    catch (error) {
      if (error?.code === 'invalid_request' && (error.message === 'unknown bridge method' || /unknown field.*backend/i.test(error.message))) {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support session liveness. Update the backend and try again.');
      }
      throw error;
    }
  }

  async request(method, params) {
    if (this.disposed) throw new BridgeError('disposed', 'Gitpane service has been disposed.');
    const initialization = this.initialization;
    if (!initialization) throw new BridgeError('not_initialized', 'Gitpane service has not been initialized.');
    await initialization;
    if (initialization !== this.initialization) throw new BridgeError('workspace_changed', 'Workspace changed before the request could start.');
    if (!this.child) throw new BridgeError('service_exited', 'Gitpane service is not running.');
    return this.call(method, params);
  }

  async status(repo, includeWorktrees, options) {
    const summary = options?.summaryOnly === true;
    try {
      return await this.request('status', { repo, ...(includeWorktrees === undefined ? {} : { include_worktrees: includeWorktrees }),
        ...(summary ? { summary_only: true, watched_paths: options.watchedPaths } : {}) });
    } catch (error) {
      if (summary && error.code === 'invalid_request' && /unknown field.*(?:summary_only|watched_paths)/i.test(error.message)) {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support compact status. Update the backend and try again.');
      }
      throw error;
    }
  }

  beginChanges(repo) { return this.request('changes_begin', { repo }); }

  nextChanges(repo, snapshot, cursor) { return this.request('changes_next', { repo, snapshot, cursor }); }

  findChange(repo, snapshot, target, group) { return this.request('changes_find', { repo, snapshot, path: target, group }); }

  releaseChanges(repo, snapshot) { return this.request('changes_release', { repo, snapshot }); }

  sortRepositories(order, repositories) {
    return this.request('sort_repositories', { order, repositories });
  }

  fileActionPreview(repo, path, operation) {
    return this.request('file_action_preview', { repo, path, operation });
  }

  async previewFetch(repo) {
    try { return await this.request('repo_fetch_preview', { repo }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support workspace fetch. Update the backend and try again.');
      }
      throw error;
    }
  }

  async validateSubmodulePin(root, chain) {
    try { return await this.request('submodule_pin_validate', { root, chain }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support submodule pins. Update the backend and try again.');
      }
      throw error;
    }
  }

  async validateRepositoryIdentity(repo, identity) {
    try { return await this.request('repo_identity_validate', { repo, expected_repo_identity: identity }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support repository identity checks. Update the backend and try again.');
      }
      throw error;
    }
  }

  async submodulePointerCompare(repo, record, identity) {
    try { return await this.request('submodule_pointer_compare', { repo, record, expected_repo_identity: identity }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support submodule pointer diffs. Update the backend and try again.');
      }
      throw error;
    }
  }

  async submodulePointerValidateBatch(repo, identity, snapshots) {
    try { return await this.request('submodule_pointer_validate_batch', { repo, expected_repo_identity: identity, snapshots }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support efficient submodule pointer checks. Update the backend and try again.');
      }
      throw error;
    }
  }

  async submodulePointerContent(repo, record, identity, headOid, sides, side) {
    try { return await this.request('submodule_pointer_content', { repo, record, expected_repo_identity: identity, head_oid: headOid, sides, side }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support submodule pointer diffs. Update the backend and try again.');
      }
      throw error;
    }
  }

  async previewWorktreeRemoval(repo, path) {
    try { return await this.request('worktree_remove_preview', { repo, path }); }
    catch (error) {
      if (error?.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support safe worktree removal. Update the backend and try again.');
      }
      throw error;
    }
  }

  readWorkingSymlink(repo, path) {
    return this.request('working_symlink', { repo, path });
  }

  planWorktree(repo, branch) {
    return this.request('worktree_plan', { repo, branch });
  }

  branchReview(repo) {
    return this.request('branch_review', { repo });
  }

  async inspectSubmodule(repo, path, summaryOnly = false, group, originalPath) {
    try {
      return await this.request('submodule_inspect', { repo, path, ...(summaryOnly ? { summary_only: true } : {}),
        ...(group === undefined ? {} : { group }), ...(originalPath === undefined ? {} : { original_path: originalPath }) });
    } catch (error) {
      if (summaryOnly && error.code === 'invalid_request' && /unknown field.*(?:summary_only|group|original_path)/i.test(error.message)) {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support selected submodule comparisons. Update the backend and try again.');
      }
      throw error;
    }
  }

  async validateSubmoduleChain(chain) {
    try { return await this.request('submodule_chain_validate', { chain }); }
    catch (error) {
      if (error.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support nested submodule validation. Update the backend and try again.');
      }
      throw error;
    }
  }

  graph(repo, firstParent = false, filters, collapsedSegments) {
    return this.request('graph', { repo, first_parent: firstParent, ...(filters === undefined ? {} : { filters }),
      ...(collapsedSegments === undefined ? {} : { collapsed_segments: collapsedSegments }) });
  }

  async graphRevision(repo) {
    try { return await this.request('graph_revision', { repo }); }
    catch (error) {
      if (error.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support graph refresh. Update the backend and try again.');
      }
      throw error;
    }
  }

  async theme() {
    try { return await this.request('theme', {}); }
    catch (error) {
      if (error.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support themes. Update the backend and try again.');
      }
      throw error;
    }
  }

  async themeRequest(method, params) {
    try { return await this.request(method, params); }
    catch (error) {
      if (error.code === 'invalid_request' && error.message === 'unknown bridge method') {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support the theme picker. Update the backend and try again.');
      }
      throw error;
    }
  }

  themesList() { return this.themeRequest('themes_list', {}); }
  themePreview(name) { return this.themeRequest('theme_preview', { name }); }

  commitFiles(repo, oid) {
    return this.request('commit_files', { repo, oid });
  }

  listLaunchers(repo) { return this.request('launchers_list', { repo }); }

  planLauncher(repo, id, fingerprint) { return this.request('launcher_plan', { repo, id, fingerprint }); }

  validateLauncher(repo, id, fingerprint) { return this.request('launcher_validate', { repo, id, fingerprint }); }

  call(method, params) {
    if (!this.child) return Promise.reject(new BridgeError('service_exited', 'Gitpane service is not running.'));
    if (this.pending.size >= MAX_PENDING) return Promise.reject(new BridgeError('busy', 'Too many Gitpane service requests are pending.'));
    if (!Number.isSafeInteger(this.nextId)) return Promise.reject(new BridgeError('protocol_error', 'Gitpane request IDs exhausted.'));
    const id = this.nextId++;
    const line = `${JSON.stringify({ id, method, params })}\n`;
    if (Buffer.byteLength(line) > MAX_REQUEST_BYTES) return Promise.reject(new BridgeError('request_too_large', 'Gitpane request exceeds the size limit.'));
    return new Promise((resolve, reject) => {
      const timer = this.timeoutMs > 0 ? setTimeout(() => {
        this.pending.delete(id);
        this.expired.add(id);
        reject(new BridgeError('timeout', `Gitpane service request timed out: ${method}.`));
        if (this.expired.size >= MAX_PENDING) {
          this.abort(this.child, new BridgeError('timeout', 'Too many Gitpane service responses timed out.'));
        }
      }, this.timeoutMs) : undefined;
      this.pending.set(id, { resolve, reject, timer });
      try { this.child.stdin.write(line); }
      catch (error) { this.abort(this.child, new BridgeError('io_error', `Could not send Gitpane request: ${error.message}`)); }
    });
  }

  receive(child, chunk) {
    if (this.child !== child) return;
    const data = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    let start = 0;
    while (start < data.length) {
      const newline = data.indexOf(10, start);
      const end = newline < 0 ? data.length : newline;
      const segment = data.subarray(start, end);
      this.lineBytes += segment.length;
      if (this.lineBytes > this.maxResponseBytes) {
        this.abort(child, new BridgeError('response_too_large', 'Gitpane service response exceeds the size limit.'));
        return;
      }
      this.parts.push(segment);
      if (newline < 0) return;
      let line;
      try { line = new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(this.parts, this.lineBytes)); }
      catch {
        this.abort(child, new BridgeError('protocol_error', 'Gitpane service returned invalid UTF-8.'));
        return;
      }
      this.parts = [];
      this.lineBytes = 0;
      if (!this.handleLine(child, line)) return;
      start = newline + 1;
    }
  }

  handleLine(child, line) {
    let response;
    try { response = JSON.parse(line); }
    catch {
      this.abort(child, new BridgeError('protocol_error', 'Gitpane service returned invalid JSON.'));
      return false;
    }
    if (!response || !Number.isSafeInteger(response.id) ||
      (Object.hasOwn(response, 'result') === Object.hasOwn(response, 'error'))) {
      this.abort(child, new BridgeError('protocol_error', 'Gitpane service returned an invalid response.'));
      return false;
    }
    if (this.expired.delete(response.id)) return true;
    const pending = this.pending.get(response.id);
    if (!pending) {
      this.abort(child, new BridgeError('protocol_error', 'Gitpane service returned an unexpected response ID.'));
      return false;
    }
    if (Object.hasOwn(response, 'error') && (!response.error || typeof response.error.code !== 'string' || typeof response.error.message !== 'string')) {
      this.abort(child, new BridgeError('protocol_error', 'Gitpane service returned an invalid error.'));
      return false;
    }
    this.pending.delete(response.id);
    clearTimeout(pending.timer);
    if (Object.hasOwn(response, 'error')) pending.reject(new BridgeError(response.error.code, response.error.message));
    else pending.resolve(response.result);
    return true;
  }

  rejectPending(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    this.expired.clear();
  }

  stop(child, graceful) {
    if (!child) return;
    if (graceful) child.stdin.end();
    else child.kill();
    const timer = setTimeout(() => child.kill('SIGKILL'), this.killGraceMs);
    timer.unref();
    child.once('close', () => clearTimeout(timer));
  }

  abort(child, error) {
    if (this.child !== child) return;
    this.child = undefined;
    this.initialization = undefined;
    this.parts = [];
    this.lineBytes = 0;
    this.rejectPending(error);
    this.stop(child, false);
    this.onFailure(error);
  }

  dispose(options = {}) {
    if (this.disposed) return;
    this.disposed = true;
    const child = this.child;
    this.child = undefined;
    this.initialization = undefined;
    this.rejectPending(new BridgeError('disposed', 'Gitpane service has been disposed.'));
    this.stop(child, !options.immediate);
    const streams = child || this.lastChild;
    if (options.closeStreams && streams) {
      streams.stdin.destroy();
      streams.stdout.destroy();
      streams.stderr.destroy();
    }
  }

  async shutdown(options = {}) {
    const closed = this.closed;
    this.dispose(options);
    if (closed) await closed;
  }
}

module.exports = { BridgeClient, BridgeError, MAX_REQUEST_BYTES };
