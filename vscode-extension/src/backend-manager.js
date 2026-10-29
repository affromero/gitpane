'use strict';

const fs = require('node:fs/promises');
const path = require('node:path');
const { BridgeClient, BridgeError } = require('./bridge-client');
const { MutationManager } = require('./mutation-manager');
const { visibleRepository } = require('./model');

class BackendManager {
  constructor(context, options = {}) {
    this.vscode = options.vscode || require('vscode');
    this.realpath = options.realpath || fs.realpath;
    this.createClient = options.createClient || ((executable, callbacks) => new BridgeClient(executable, callbacks));
    this.output = options.output;
    this.sessionMutations = options.sessionMutations || new MutationManager({
      getSnapshot: () => this.mutationSnapshot(),
      isCurrent: (snapshot, repo) => this.isCurrentMutation(snapshot, repo),
      onStderr: chunk => this.output?.append(chunk)
    });
    this.extensionPath = context.extensionPath;
    this.client = undefined;
    this.executable = undefined;
    this.roots = undefined;
    this.generation = 0;
    this.disposed = false;
    this.emitter = new this.vscode.EventEmitter();
    this.onDidChange = this.emitter.event;
    this.state = { status: 'initializing' };
    this.subscriptions = [
      this.vscode.workspace.onDidChangeWorkspaceFolders(() => {
        this.client?.dispose({ immediate: true });
        this.client = undefined;
        this.executable = undefined;
        this.reconfigure();
      }),
      this.vscode.workspace.onDidChangeConfiguration(event => {
        if (event.affectsConfiguration('gitpane.backendPath')) this.reconfigure();
      })
    ];
    this.reconfigure();
  }

  setState(status, error) {
    if (this.disposed) return;
    this.state = error ? { status, error } : { status };
    this.emitter.fire(this.state);
  }

  reconfigure() {
    if (this.disposed) return Promise.reject(new BridgeError('disposed', 'Gitpane backend has been disposed.'));
    const generation = ++this.generation;
    this.roots = undefined;
    this.setState('initializing');
    const configured = this.configure(generation);
    this.configuration = configured;
    configured.catch(error => {
      if (!this.disposed && generation === this.generation) this.setState('error', error);
    });
    return configured;
  }

  async configure(generation) {
    const configured = this.vscode.workspace.getConfiguration('gitpane').get('backendPath', '');
    if (typeof configured !== 'string' || (configured && !path.isAbsolute(configured))) {
      throw new BridgeError('invalid_backend_path', 'Gitpane backend path must be absolute.');
    }
    const executable = configured || path.join(this.extensionPath, 'bin', process.platform === 'win32' ? 'gitpane.exe' : 'gitpane');
    if (!path.isAbsolute(executable)) throw new BridgeError('invalid_backend_path', 'Gitpane backend path must be absolute.');
    const folders = (this.vscode.workspace.workspaceFolders || []).filter(folder => folder.uri.scheme === 'file');
    const roots = [...new Set(await Promise.all(folders.map(folder => this.realpath(folder.uri.fsPath))))];
    if (this.disposed || generation !== this.generation) return;
    if (!this.client || executable !== this.executable) {
      this.client?.dispose();
      const callbacks = {
        onStderr: chunk => this.output?.append(chunk),
        onFailure: error => {
          if (!this.disposed && this.client === client) this.setState('error', error);
        }
      };
      const client = this.createClient(executable, callbacks);
      this.client = client;
      this.executable = executable;
    }
    const client = this.client;
    await client.initialize(roots);
    if (this.disposed || generation !== this.generation || client !== this.client) return;
    this.roots = roots;
    this.setState('ready');
  }

  async mutationSnapshot() {
    await this.ready();
    if (!this.client || !this.executable || !this.roots) {
      throw new BridgeError('invalid_workspace', 'Gitpane workspace scope is unavailable.');
    }
    return { executable: this.executable, roots: [...this.roots], generation: this.generation };
  }

  isCurrentMutation(snapshot, repo) {
    return !this.disposed && !!this.roots && !!snapshot && typeof repo === 'string' &&
      snapshot.generation === this.generation &&
      snapshot.executable === this.executable && Array.isArray(snapshot.roots) &&
      snapshot.roots.length === this.roots.length &&
      snapshot.roots.every((root, index) => root === this.roots[index]) &&
      visibleRepository(repo, this.roots);
  }

  async ready() {
    while (!this.disposed) {
      const generation = this.generation;
      try { await this.configuration; }
      catch (error) {
        if (generation !== this.generation) continue;
        throw error;
      }
      if (generation === this.generation) return;
    }
    throw new BridgeError('disposed', 'Gitpane backend has been disposed.');
  }

  async request(method, repo, argument, filters, collapsedSegments) {
    await this.ready();
    const generation = this.generation;
    const client = this.client;
    try {
      let result;
      if (method === 'status') result = await client.status(repo);
      else if (method === 'sort_repositories') result = await client.sortRepositories(argument.order, argument.repositories);
      else if (method === 'file_action_preview') result = await client.fileActionPreview(repo, argument.path, argument.operation);
      else if (method === 'branch_review') result = await client.branchReview(repo);
      else if (method === 'submodule_inspect') result = await client.inspectSubmodule(repo, argument);
      else if (method === 'graph') result = await client.graph(repo, argument, filters, collapsedSegments);
      else if (method === 'commit_files') result = await client.commitFiles(repo, argument);
      else if (method === 'launchers_list') result = await client.listLaunchers(repo);
      else if (method === 'launcher_plan') result = await client.planLauncher(repo, argument.id, argument.fingerprint);
      else if (method === 'launcher_validate') result = await client.validateLauncher(repo, argument.id, argument.fingerprint);
      else throw new BridgeError('invalid_request', 'Unsupported Gitpane backend request.');
      if (this.disposed || generation !== this.generation || client !== this.client) {
        throw new BridgeError('workspace_changed', 'Workspace changed before the Gitpane response arrived.');
      }
      if (this.state.status === 'error') this.setState('ready');
      return result;
    } catch (error) {
      if (!this.disposed && generation !== this.generation) {
        throw new BridgeError('workspace_changed', 'Workspace changed before the Gitpane response arrived.');
      }
      if (!this.disposed && generation === this.generation && client === this.client) this.setState('error', error);
      throw error;
    }
  }

  status(repo) { return this.request('status', repo); }

  sortRepositories(order, repositories) { return this.request('sort_repositories', undefined, { order, repositories }); }

  fileActionPreview(repo, path, operation) { return this.request('file_action_preview', repo, { path, operation }); }

  branchReview(repo) { return this.request('branch_review', repo); }

  inspectSubmodule(repo, path) { return this.request('submodule_inspect', repo, path); }

  async listSessions(repo, backend) {
    if (!['tmux', 'herdr'].includes(backend)) throw new BridgeError('invalid_request', 'Unknown session backend.');
    const snapshot = await this.mutationSnapshot();
    const repoPath = await this.realpath(repo);
    if (!this.isCurrentMutation(snapshot, repoPath)) throw new BridgeError('workspace_changed', 'Workspace changed before finding sessions.');
    const client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk) });
    try {
      await client.initialize(snapshot.roots);
      if (!this.isCurrentMutation(snapshot, repoPath)) throw new BridgeError('workspace_changed', 'Workspace changed before finding sessions.');
      const result = await client.call('sessions_list', { repo: repoPath, backend });
      if (!this.isCurrentMutation(snapshot, repoPath) || await this.realpath(repo) !== repoPath) {
        throw new BridgeError('workspace_changed', 'Workspace changed while finding sessions.');
      }
      return result;
    } finally { client.dispose(); }
  }

  async gotoSession(repo, backend, session, validateTarget) {
    if (!['tmux', 'herdr'].includes(backend) || typeof session !== 'string' || !session || typeof validateTarget !== 'function') {
      throw new BridgeError('invalid_request', 'A current session and repository are required.');
    }
    const status = await this.status(repo);
    return this.sessionMutations.execute('session_goto', { repo, backend, session }, {
      lockKey: status?.common_dir, validateTarget
    });
  }

  graph(repo, firstParent = false, filters, collapsedSegments) { return this.request('graph', repo, firstParent, filters, collapsedSegments); }

  commitFiles(repo, oid) { return this.request('commit_files', repo, oid); }

  listLaunchers(repo) { return this.request('launchers_list', repo); }

  planLauncher(repo, id, fingerprint) { return this.request('launcher_plan', repo, { id, fingerprint }); }

  validateLauncher(repo, id, fingerprint) { return this.request('launcher_validate', repo, { id, fingerprint }); }

  retry() { return this.reconfigure(); }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.generation++;
    for (const subscription of this.subscriptions) subscription.dispose();
    this.client?.dispose();
    this.sessionMutations.dispose();
    this.client = undefined;
    this.state = { status: 'disposed' };
    this.emitter.dispose();
  }
}

module.exports = { BackendManager };
