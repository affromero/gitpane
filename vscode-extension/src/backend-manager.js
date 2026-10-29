'use strict';

const fs = require('node:fs/promises');
const path = require('node:path');
const { BridgeClient, BridgeError } = require('./bridge-client');
const { MutationManager } = require('./mutation-manager');
const { visibleRepository } = require('./model');

function normalizeWorkspacePath(value, platform = process.platform) {
  if (platform !== 'win32') return value;
  const ordinary = value.replace(/^\\\\\?\\UNC\\/i, '\\\\').replace(/^\\\\\?\\/, '');
  return path.win32.normalize(ordinary).toLowerCase();
}

function sameWorkspaceScope(first, second) {
  return !!first && !!second && first.roots.length === second.roots.length &&
    first.roots.every((root, index) => root === second.roots[index] && first.sources[index] === second.sources[index]);
}

class BackendManager {
  constructor(context, options = {}) {
    this.vscode = options.vscode || require('vscode');
    this.realpath = options.realpath || fs.realpath;
    this.stat = options.stat || fs.stat;
    this.createClient = options.createClient || ((executable, callbacks) => new BridgeClient(executable, callbacks));
    this.output = options.output;
    this.sessionMutations = options.sessionMutations || new MutationManager({
      getSnapshot: () => this.mutationSnapshot(),
      isCurrent: (snapshot, repo) => this.isCurrentMutation(snapshot, repo),
      onStderr: chunk => this.output?.append(chunk)
    });
    this.extensionPath = context.extensionPath;
    this.client = undefined;
    this.changesClient = undefined;
    this.changesConfiguration = undefined;
    this.changesEpoch = 0;
    this.validationClient = undefined;
    this.validationConfiguration = undefined;
    this.validationEpoch = 0;
    this.pinValidationEpoch = 0;
    this.pinValidationClient = undefined;
    this.activeValidations = 0;
    this.verifyClients = new Set();
    this.livenessClient = undefined;
    this.livenessEpoch = 0;
    this.livenessInitializationTimeoutMs = options.livenessInitializationTimeoutMs ?? 10000;
    this.livenessRequestTimeoutMs = options.livenessRequestTimeoutMs ?? 45000;
    this.executable = undefined;
    this.roots = undefined;
    this.rootSources = undefined;
    this.rootIdentities = undefined;
    this.workspaceIdentities = new Map();
    this.generation = 0;
    this.disposed = false;
    this.emitter = new this.vscode.EventEmitter();
    this.onDidChange = this.emitter.event;
    this.state = { status: 'initializing' };
    this.subscriptions = [
      this.vscode.workspace.onDidChangeWorkspaceFolders(() => {
        const active = new Set((this.vscode.workspace.workspaceFolders || []).filter(folder => folder.uri.scheme === 'file').map(folder => folder.uri.fsPath));
        for (const source of this.workspaceIdentities.keys()) if (!active.has(source)) this.workspaceIdentities.delete(source);
        this.client?.dispose({ immediate: true });
        this.client = undefined;
        this.executable = undefined;
        this.reconfigure();
      }),
      this.vscode.workspace.onDidChangeConfiguration(event => {
        if (event.affectsConfiguration('gitpane.backendPath')) this.reconfigure();
        if (event.affectsConfiguration('gitpane.sessionBackend')) {
          this.cancelSessionLiveness();
          this.emitter.fire({ ...this.state, livenessChanged: true });
        }
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
    this.cancelChanges();
    this.cancelValidation();
    this.cancelSessionLiveness();
    for (const client of this.verifyClients) client.dispose({ immediate: true });
    this.verifyClients.clear();
    this.roots = undefined;
    this.rootSources = undefined;
    this.rootIdentities = undefined;
    this.setState('initializing');
    const configured = this.configure(generation);
    this.configuration = configured;
    configured.catch(error => {
      if (!this.disposed && generation === this.generation) this.setState('error', error);
    });
    return configured;
  }

  async workspaceRoots(generation) {
    const folders = (this.vscode.workspace.workspaceFolders || []).filter(folder => folder.uri.scheme === 'file');
    const identities = await Promise.all(folders.map(async folder => {
      try {
        const resolved = await this.realpath(folder.uri.fsPath);
        const stat = await this.stat(resolved, { bigint: true });
        return stat.isDirectory() ? { path: normalizeWorkspacePath(resolved), dev: stat.dev, ino: stat.ino } : undefined;
      }
      catch (error) {
        if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') return undefined;
        throw error;
      }
    }));
    if (this.disposed || generation !== this.generation) return undefined;
    const paths = [];
    const sources = [];
    folders.forEach((folder, index) => {
      const source = folder.uri.fsPath;
      const current = identities[index];
      if (!this.workspaceIdentities.has(source)) this.workspaceIdentities.set(source, current);
      const saved = this.workspaceIdentities.get(source);
      if (saved && current && saved.path === current.path && saved.dev === current.dev && saved.ino === current.ino) {
        paths.push(current.path);
        sources.push(source);
      }
    });
    return { roots: paths, sources };
  }

  async syncWorkspaceRoots() {
    while (!this.disposed) {
      await this.ready();
      const generation = this.generation;
      const previous = this.roots && this.rootSources ? { roots: this.roots, sources: this.rootSources } : undefined;
      const current = await this.workspaceRoots(generation);
      if (generation !== this.generation) continue;
      if (!previous || !current) throw new BridgeError('invalid_workspace', 'Gitpane workspace scope is unavailable.');
      if (sameWorkspaceScope(previous, current)) return;
      const nextGeneration = generation + 1;
      try { await this.reconfigure(); }
      catch (error) {
        if (nextGeneration !== this.generation) continue;
        throw error;
      }
    }
    throw new BridgeError('disposed', 'Gitpane backend has been disposed.');
  }

  async configure(generation) {
    const configured = this.vscode.workspace.getConfiguration('gitpane').get('backendPath', '');
    if (typeof configured !== 'string' || (configured && !path.isAbsolute(configured))) {
      throw new BridgeError('invalid_backend_path', 'Gitpane backend path must be absolute.');
    }
    const executable = configured || path.join(this.extensionPath, 'bin', process.platform === 'win32' ? 'gitpane.exe' : 'gitpane');
    if (!path.isAbsolute(executable)) throw new BridgeError('invalid_backend_path', 'Gitpane backend path must be absolute.');
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
    for (let attempt = 0; attempt < 3; attempt++) {
      const scope = await this.workspaceRoots(generation);
      if (this.disposed || generation !== this.generation || !scope || client !== this.client) return;
      const { roots, sources } = scope;
      const known = sources.map(source => this.workspaceIdentities.get(source)?.token);
      try {
        await client.initialize(roots, known.every(token => token !== undefined) ? known : undefined, sources);
        const result = await client.workspaceRootIdentities();
        if (!result || !Array.isArray(result.roots) || result.roots.length !== roots.length || result.roots.some((item, index) =>
          !item || typeof item.path !== 'string' || normalizeWorkspacePath(item.path) !== roots[index] || item.source !== sources[index] ||
          typeof item.identity !== 'string' || !/^[a-f0-9]{64}$/.test(item.identity))) {
          throw new BridgeError('protocol_error', 'Gitpane returned invalid workspace identities. Update the backend and try again.');
        }
        const current = await this.workspaceRoots(generation);
        if (this.disposed || generation !== this.generation || client !== this.client) return;
        if (!sameWorkspaceScope(current, scope)) continue;
        for (let index = 0; index < roots.length; index++) {
          const token = result.roots[index].identity;
          if (known[index] !== undefined && known[index] !== token) {
            throw new BridgeError('workspace_changed', 'Workspace folder identity changed. Remove and re-add it to the workspace.');
          }
          this.workspaceIdentities.get(sources[index]).token = token;
        }
        this.roots = roots;
        this.rootSources = sources;
        this.rootIdentities = result.roots.map(item => item.identity);
        this.setState('ready');
        return;
      } catch (error) {
        if (this.disposed || generation !== this.generation || client !== this.client) return;
        const current = await this.workspaceRoots(generation);
        if (generation !== this.generation || this.disposed) return;
        if (!current || sameWorkspaceScope(current, scope)) throw error;
        if (attempt === 2) throw new BridgeError('workspace_changed', 'Workspace folders kept changing during initialization. Refresh and try again.');
      }
    }
    throw new BridgeError('workspace_changed', 'Workspace folders kept changing during initialization. Refresh and try again.');
  }

  async mutationSnapshot() {
    await this.syncWorkspaceRoots();
    if (!this.client || !this.executable || !this.roots || !this.rootSources || !this.rootIdentities) {
      throw new BridgeError('invalid_workspace', 'Gitpane workspace scope is unavailable.');
    }
    return { executable: this.executable, roots: [...this.roots], rootSources: [...this.rootSources], rootIdentities: [...this.rootIdentities], generation: this.generation };
  }

  isCurrentMutation(snapshot, repo) {
    return !this.disposed && !!this.roots && !!snapshot && typeof repo === 'string' &&
      snapshot.generation === this.generation &&
      snapshot.executable === this.executable && Array.isArray(snapshot.roots) &&
      snapshot.roots.length === this.roots.length &&
      snapshot.roots.every((root, index) => root === this.roots[index]) && Array.isArray(snapshot.rootIdentities) &&
      !!this.rootSources && Array.isArray(snapshot.rootSources) && snapshot.rootSources.length === this.rootSources.length &&
      snapshot.rootSources.every((source, index) => source === this.rootSources[index]) &&
      !!this.rootIdentities && snapshot.rootIdentities.length === this.rootIdentities.length &&
      snapshot.rootIdentities.every((token, index) => token === this.rootIdentities[index]) &&
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
    const isChanges = method.startsWith('changes_');
    const changesEpoch = this.changesEpoch;
    await this.ready();
    const generation = this.generation;
    const client = isChanges ? await this.getChangesClient(changesEpoch) : this.client;
    const current = () => isChanges ? client === this.changesClient : client === this.client;
    try {
      if (isChanges && changesEpoch !== this.changesEpoch) {
        throw new BridgeError('workspace_changed', 'Change scan was cancelled.');
      }
      let result;
      if (method === 'status') result = await client.status(repo, argument.includeWorktrees, argument.options);
      else if (method === 'repository_preview') result = await client.status(repo, false, { summaryOnly: true, watchedPaths: [] });
      else if (method === 'changes_begin') result = await client.beginChanges(repo);
      else if (method === 'changes_next') result = await client.nextChanges(repo, argument.snapshot, argument.cursor);
      else if (method === 'changes_release') result = await client.releaseChanges(repo, argument);
      else if (method === 'sort_repositories') result = await client.sortRepositories(argument.order, argument.repositories);
      else if (method === 'file_action_preview') result = await client.fileActionPreview(repo, argument.path, argument.operation);
      else if (method === 'worktree_remove_preview') result = await client.previewWorktreeRemoval(repo, argument);
      else if (method === 'working_symlink') result = await client.readWorkingSymlink(repo, argument);
      else if (method === 'worktree_plan') result = await client.planWorktree(repo, argument);
      else if (method === 'branch_review') result = await client.branchReview(repo);
      else if (method === 'submodule_inspect') result = await client.inspectSubmodule(repo, argument.path, true, argument.group, argument.originalPath);
      else if (method === 'repo_identity_validate') result = await client.validateRepositoryIdentity(repo, argument);
      else if (method === 'graph') result = await client.graph(repo, argument, filters, collapsedSegments);
      else if (method === 'graph_revision') result = await client.graphRevision(repo);
      else if (method === 'theme') result = await client.theme();
      else if (method === 'commit_files') result = await client.commitFiles(repo, argument);
      else if (method === 'launchers_list') result = await client.listLaunchers(repo);
      else if (method === 'launcher_plan') result = await client.planLauncher(repo, argument.id, argument.fingerprint);
      else if (method === 'launcher_validate') result = await client.validateLauncher(repo, argument.id, argument.fingerprint);
      else throw new BridgeError('invalid_request', 'Unsupported Gitpane backend request.');
      if (this.disposed || generation !== this.generation || !current()) {
        throw new BridgeError('workspace_changed', 'Workspace changed before the Gitpane response arrived.');
      }
      if (!isChanges && this.state.status === 'error') this.setState('ready');
      return result;
    } catch (error) {
      if (!this.disposed && generation !== this.generation) {
        throw new BridgeError('workspace_changed', 'Workspace changed before the Gitpane response arrived.');
      }
      const invalidWorktreePlan = method === 'worktree_plan' && error?.code === 'invalid_request';
      const invalidWorktreePreview = method === 'worktree_remove_preview' && ['invalid_request', 'unsupported_backend', 'stale_target', 'outside_workspace', 'repository_error'].includes(error?.code);
      const invalidLink = method === 'working_symlink' &&
        ['invalid_request', 'repository_error', 'outside_workspace', 'stale_target', 'file_changed', 'file_too_large'].includes(error?.code);
      const invalidSubmodule = method === 'submodule_inspect' &&
        ['invalid_request', 'repository_error', 'outside_workspace', 'stale_target'].includes(error?.code);
      const invalidIdentity = method === 'repo_identity_validate' &&
        ['invalid_request', 'repository_error', 'outside_workspace', 'stale_target', 'unsupported_backend'].includes(error?.code);
      const invalidRepositoryPreview = method === 'repository_preview' &&
        ['invalid_request', 'repository_error', 'outside_workspace', 'stale_target', 'unsupported_backend'].includes(error?.code);
      const staleChanges = ['changes_next', 'changes_release'].includes(method) && error?.code === 'snapshot_stale';
      const obsoleteRelease = method === 'changes_release' && ['outside_workspace', 'workspace_changed'].includes(error?.code);
      const invalidTheme = method === 'theme' && ['theme_error', 'invalid_request', 'unsupported_backend'].includes(error?.code);
      if (!isChanges && !this.disposed && generation === this.generation && current() && !invalidWorktreePlan && !invalidWorktreePreview && !invalidLink && !invalidSubmodule && !invalidIdentity && !invalidRepositoryPreview && !staleChanges && !obsoleteRelease && !invalidTheme) {
        this.setState('error', error);
      }
      throw error;
    }
  }

  async getChangesClient(epoch) {
    await this.ready();
    if (this.disposed || epoch !== this.changesEpoch) {
      throw new BridgeError('workspace_changed', 'Change scan was cancelled.');
    }
    if (this.changesConfiguration) return this.changesConfiguration;
    const snapshot = await this.mutationSnapshot();
    if (this.disposed || epoch !== this.changesEpoch) {
      throw new BridgeError('workspace_changed', 'Change scan was cancelled.');
    }
    if (this.changesConfiguration) return this.changesConfiguration;
    const client = this.createClient(snapshot.executable, {
      onStderr: chunk => this.output?.append(chunk),
      onFailure: () => {
        if (client === this.changesClient) { this.changesClient = undefined; this.changesConfiguration = undefined; }
      }
    });
    this.changesClient = client;
    const pending = (async () => {
      try {
        await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
        if (this.disposed || epoch !== this.changesEpoch || snapshot.generation !== this.generation || client !== this.changesClient) {
          throw new BridgeError('workspace_changed', 'Workspace changed before changes could load.');
        }
        return client;
      } catch (error) {
        client.dispose({ immediate: true });
        if (client === this.changesClient) { this.changesClient = undefined; this.changesConfiguration = undefined; }
        throw error;
      }
    })();
    this.changesConfiguration = pending;
    return pending;
  }

  cancelChanges() {
    this.changesEpoch++;
    this.changesClient?.dispose({ immediate: true });
    this.changesClient = undefined;
    this.changesConfiguration = undefined;
  }

  cancelValidation() {
    this.validationEpoch++;
    this.cancelPinValidation();
    this.validationClient?.dispose({ immediate: true });
    this.validationClient = undefined;
    this.validationConfiguration = undefined;
  }

  cancelPinValidation() {
    this.pinValidationEpoch++;
    this.pinValidationClient?.dispose({ immediate: true, closeStreams: true });
  }

  async getValidationClient(epoch) {
    await this.ready();
    if (this.disposed || epoch !== this.validationEpoch) throw new BridgeError('workspace_changed', 'Submodule validation was cancelled.');
    if (this.validationConfiguration) return this.validationConfiguration;
    const snapshot = await this.mutationSnapshot();
    if (this.disposed || epoch !== this.validationEpoch) throw new BridgeError('workspace_changed', 'Submodule validation was cancelled.');
    if (this.validationConfiguration) return this.validationConfiguration;
    const client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk),
      onFailure: () => {
        if (client === this.validationClient) { this.validationClient = undefined; this.validationConfiguration = undefined; }
      } });
    this.validationClient = client;
    const pending = (async () => {
      try {
        await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
        if (this.disposed || epoch !== this.validationEpoch || snapshot.generation !== this.generation || client !== this.validationClient) {
          throw new BridgeError('workspace_changed', 'Workspace changed before submodule validation.');
        }
        return client;
      } catch (error) {
        client.dispose({ immediate: true });
        if (client === this.validationClient) { this.validationClient = undefined; this.validationConfiguration = undefined; }
        throw error;
      }
    })();
    this.validationConfiguration = pending;
    return pending;
  }

  async validateSubmoduleChain(chain) {
    if (!Array.isArray(chain) || chain.length > 32 || chain.some(link => !link ||
        typeof link.repo !== 'string' || !path.isAbsolute(link.repo) ||
        typeof link.child_repo !== 'string' || !path.isAbsolute(link.child_repo))) {
      throw new BridgeError('invalid_request', 'Invalid submodule validation chain.');
    }
    if (this.activeValidations >= 4) throw new BridgeError('busy', 'Too many submodule checks are running. Try again.');
    this.activeValidations++;
    const epoch = this.validationEpoch;
    try {
      const snapshot = await this.mutationSnapshot();
      const paths = await Promise.all(chain.flatMap(link => [link.repo, link.child_repo]).map(value => this.realpath(value)));
      const current = () => epoch === this.validationEpoch && paths.every(value => this.isCurrentMutation(snapshot, value));
      if (!current()) throw new BridgeError('workspace_changed', 'Workspace changed before submodule validation.');
      const client = await this.getValidationClient(epoch);
      const result = await client.validateSubmoduleChain(chain);
      const resolved = await Promise.all(chain.flatMap(link => [link.repo, link.child_repo]).map(value => this.realpath(value)));
      if (!current() || client !== this.validationClient || paths.some((value, index) => value !== resolved[index])) {
        throw new BridgeError('workspace_changed', 'Workspace changed while validating submodules.');
      }
      if (!result || typeof result.valid !== 'boolean') throw new BridgeError('protocol_error', 'Gitpane returned an invalid submodule validation result.');
      return result;
    } catch (error) {
      if (this.disposed || epoch !== this.validationEpoch) throw new BridgeError('workspace_changed', 'Submodule validation was cancelled.');
      if (error?.code === 'ENOENT') return { valid: false };
      throw error;
    } finally { this.activeValidations--; }
  }

  async validateSubmodulePin(root, chain) {
    if (typeof root !== 'string' || !path.isAbsolute(root) || !Array.isArray(chain) || !chain.length || chain.length > 32) {
      throw new BridgeError('invalid_request', 'Invalid submodule pin.');
    }
    if (this.activeValidations >= 4) throw new BridgeError('busy', 'Too many submodule checks are running. Try again.');
    this.activeValidations++;
    const epoch = this.validationEpoch;
    try {
      const snapshot = await this.mutationSnapshot();
      const resolved = await this.realpath(root);
      if (epoch !== this.validationEpoch || !this.isCurrentMutation(snapshot, resolved)) {
        throw new BridgeError('workspace_changed', 'Workspace changed before validating the submodule pin.');
      }
      const client = await this.getValidationClient(epoch);
      if (epoch !== this.validationEpoch || !this.isCurrentMutation(snapshot, resolved)) throw new BridgeError('workspace_changed', 'Submodule pin validation was cancelled.');
      const result = await client.validateSubmodulePin(root, chain);
      if (epoch !== this.validationEpoch || client !== this.validationClient || !this.isCurrentMutation(snapshot, resolved) ||
        await this.realpath(root) !== resolved || !this.isCurrentMutation(snapshot, resolved)) {
        throw new BridgeError('workspace_changed', 'Workspace changed while validating the submodule pin.');
      }
      return result;
    } finally { this.activeValidations--; }
  }

  async openPinValidationSession() {
    if (this.activeValidations >= 4 || this.pinValidationClient) throw new BridgeError('busy', 'A pinned submodule check is already running.');
    this.activeValidations++;
    const epoch = this.validationEpoch;
    const pinEpoch = this.pinValidationEpoch;
    let client;
    let closing;
    const close = () => {
      if (closing) return closing;
      closing = (async () => {
        if (client) await client.shutdown({ immediate: true, closeStreams: true });
        if (this.pinValidationClient === client) this.pinValidationClient = undefined;
        this.activeValidations--;
      })();
      return closing;
    };
    try {
      const snapshot = await this.mutationSnapshot();
      if (this.disposed || epoch !== this.validationEpoch || pinEpoch !== this.pinValidationEpoch) {
        throw new BridgeError('workspace_changed', 'Submodule pin validation was cancelled.');
      }
      if (this.pinValidationClient) throw new BridgeError('busy', 'A pinned submodule check is already running.');
      client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk), timeoutMs: 30000 });
      this.pinValidationClient = client;
      await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
      const current = () => !closing && !this.disposed && epoch === this.validationEpoch && pinEpoch === this.pinValidationEpoch &&
        client === this.pinValidationClient && snapshot.generation === this.generation;
      if (!current()) throw new BridgeError('workspace_changed', 'Submodule pin validation was cancelled.');
      return { close, validate: async (root, chain) => {
        if (typeof root !== 'string' || !path.isAbsolute(root) || !Array.isArray(chain) || !chain.length || chain.length > 32) {
          throw new BridgeError('invalid_request', 'Invalid submodule pin.');
        }
        const resolved = await this.realpath(root);
        if (!current() || !this.isCurrentMutation(snapshot, resolved)) throw new BridgeError('workspace_changed', 'Workspace changed before validating the submodule pin.');
        const result = await client.validateSubmodulePin(root, chain);
        const after = await this.realpath(root);
        if (!current() || !this.isCurrentMutation(snapshot, resolved) || after !== resolved) {
          throw new BridgeError('workspace_changed', 'Workspace changed while validating the submodule pin.');
        }
        return result;
      } };
    } catch (error) { await close(); throw error; }
  }

  async openChangesSession(repo) {
    const snapshot = await this.mutationSnapshot();
    const repoPath = await this.realpath(repo);
    if (!this.isCurrentMutation(snapshot, repoPath)) throw new BridgeError('workspace_changed', 'Workspace changed before checking the file.');
    if (this.verifyClients.size >= 4) throw new BridgeError('busy', 'Too many file checks are running. Try again.');
    const client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk) });
    this.verifyClients.add(client);
    let closed = false;
    const close = () => {
      if (closed) return;
      closed = true;
      this.verifyClients.delete(client);
      client.dispose({ immediate: true });
    };
    const current = async () => {
      if (closed || !this.isCurrentMutation(snapshot, repoPath)) return false;
      const resolved = await this.realpath(repo);
      return !closed && this.isCurrentMutation(snapshot, repoPath) && resolved === repoPath;
    };
    const run = async operation => {
      if (!await current()) throw new BridgeError('workspace_changed', 'Workspace changed before checking the file.');
      const result = await operation();
      if (!await current()) throw new BridgeError('workspace_changed', 'Workspace changed while checking the file.');
      return result;
    };
    try {
      await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
      if (!await current()) throw new BridgeError('workspace_changed', 'Workspace changed before checking the file.');
      return {
        begin: () => run(() => client.beginChanges(repoPath)),
        next: (id, cursor) => run(() => client.nextChanges(repoPath, id, cursor)),
        find: (id, record) => run(() => client.findChange(repoPath, id, record.path, record.group)),
        close
      };
    } catch (error) { close(); throw error; }
  }

  async verifyChanges(repo, record) {
    const session = await this.openChangesSession(repo);
    try {
      const page = await session.begin();
      if (!page || typeof page.snapshot !== 'string' || !record || typeof record.path !== 'string' || typeof record.group !== 'string') {
        throw new BridgeError('protocol_error', 'Gitpane returned an invalid change snapshot.');
      }
      const result = await session.find(page.snapshot, record);
      return { page, change: result?.change };
    } finally { session.close(); }
  }

  status(repo, includeWorktrees, options) { return this.request('status', repo, { includeWorktrees, options }); }

  previewRepository(repo) { return this.request('repository_preview', repo); }

  beginChanges(repo) { return this.request('changes_begin', repo); }

  nextChanges(repo, snapshot, cursor) { return this.request('changes_next', repo, { snapshot, cursor }); }

  releaseChanges(repo, snapshot) { return this.request('changes_release', repo, snapshot); }

  sortRepositories(order, repositories) { return this.request('sort_repositories', undefined, { order, repositories }); }

  fileActionPreview(repo, path, operation) { return this.request('file_action_preview', repo, { path, operation }); }

  async previewFetch(repo) {
    const snapshot = await this.mutationSnapshot();
    const repoPath = await this.realpath(repo);
    if (!this.isCurrentMutation(snapshot, repoPath)) throw new BridgeError('workspace_changed', 'Workspace changed before checking fetch.');
    const client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk), timeoutMs: 45000 });
    try {
      await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
      if (!this.isCurrentMutation(snapshot, repoPath)) throw new BridgeError('workspace_changed', 'Workspace changed before checking fetch.');
      const result = await client.previewFetch(repoPath);
      const currentPath = await this.realpath(repo);
      if (!this.isCurrentMutation(snapshot, repoPath) || currentPath !== repoPath) {
        throw new BridgeError('workspace_changed', 'Workspace changed while checking fetch.');
      }
      return result;
    } finally { await client.shutdown(); }
  }

  previewWorktreeRemoval(repo, path) { return this.request('worktree_remove_preview', repo, path); }

  readWorkingSymlink(repo, path) { return this.request('working_symlink', repo, path); }

  planWorktree(repo, branch) { return this.request('worktree_plan', repo, branch); }

  branchReview(repo) { return this.request('branch_review', repo); }

  inspectSubmodule(repo, path, group, originalPath) { return this.request('submodule_inspect', repo, { path, group, originalPath }); }

  validateRepositoryIdentity(repo, identity) { return this.request('repo_identity_validate', repo, identity); }

  cancelSessionLiveness() {
    this.livenessEpoch++;
  }

  async sessionLiveness(repos) {
    if (!Array.isArray(repos) || repos.length > 1024 || repos.some(repo => typeof repo !== 'string' || !path.isAbsolute(repo))) {
      throw new BridgeError('invalid_request', 'Invalid session liveness repositories.');
    }
    const epoch = this.livenessEpoch;
    const selection = this.vscode.workspace.getConfiguration('gitpane').get('sessionBackend', 'auto');
    if (!['auto', 'tmux', 'herdr'].includes(selection)) throw new BridgeError('invalid_request', 'Choose auto, tmux, or herdr for gitpane.sessionBackend.');
    const snapshot = await this.mutationSnapshot();
    if (epoch !== this.livenessEpoch || this.disposed) throw new BridgeError('workspace_changed', 'Session liveness check was cancelled.');
    if (this.livenessClient) throw new BridgeError('busy', 'A session liveness check is already running.');
    if (repos.some(repo => !this.isCurrentMutation(snapshot, repo))) throw new BridgeError('outside_workspace', 'Session liveness repository is outside the workspace.');
    const client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk),
      timeoutMs: this.livenessInitializationTimeoutMs, serviceArgs: ['serve', '--liveness-worker'], killGraceMs: 3000 });
    this.livenessClient = client;
    let initialized = false;
    let completedProbe = false;
    try {
      const handshake = await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
      if (handshake?.capabilities?.liveness_worker !== true) {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend lacks safe session liveness support. Update the backend and try again.');
      }
      initialized = true;
      if (epoch !== this.livenessEpoch || this.disposed || snapshot.generation !== this.generation) throw new BridgeError('workspace_changed', 'Session liveness check was cancelled.');
      client.timeoutMs = this.livenessRequestTimeoutMs;
      const result = await client.sessionLiveness(repos, selection);
      completedProbe = true;
      if (epoch !== this.livenessEpoch || this.disposed || snapshot.generation !== this.generation) throw new BridgeError('workspace_changed', 'Session liveness check was cancelled.');
      return result;
    } catch (error) {
      if (epoch !== this.livenessEpoch || this.disposed || snapshot.generation !== this.generation) {
        throw new BridgeError('workspace_changed', 'Session liveness check was cancelled.');
      }
      if (!initialized && error?.code === 'service_exited') {
        throw new BridgeError('unsupported_backend', 'Gitpane could not start a dedicated session liveness worker. Update or check the configured backend.');
      }
      throw error;
    } finally {
      await client.shutdown({ immediate: !completedProbe });
      if (this.livenessClient === client) this.livenessClient = undefined;
    }
  }

  async listSessions(repo, backend) {
    if (!['tmux', 'herdr'].includes(backend)) throw new BridgeError('invalid_request', 'Unknown session backend.');
    const snapshot = await this.mutationSnapshot();
    const repoPath = await this.realpath(repo);
    if (!this.isCurrentMutation(snapshot, repoPath)) throw new BridgeError('workspace_changed', 'Workspace changed before finding sessions.');
    const client = this.createClient(snapshot.executable, { onStderr: chunk => this.output?.append(chunk) });
    try {
      await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
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
    const status = await this.status(repo, undefined, { summaryOnly: true, watchedPaths: [] });
    return this.sessionMutations.execute('session_goto', { repo, backend, session }, {
      lockKey: status?.common_dir, validateTarget
    });
  }

  graph(repo, firstParent = false, filters, collapsedSegments) { return this.request('graph', repo, firstParent, filters, collapsedSegments); }

  graphRevision(repo) { return this.request('graph_revision', repo); }

  theme() { return this.request('theme'); }

  commitFiles(repo, oid) { return this.request('commit_files', repo, oid); }

  listLaunchers(repo) { return this.request('launchers_list', repo); }

  planLauncher(repo, id, fingerprint) { return this.request('launcher_plan', repo, { id, fingerprint }); }

  validateLauncher(repo, id, fingerprint) { return this.request('launcher_validate', repo, { id, fingerprint }); }

  retry() { return this.reconfigure(); }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.cancelSessionLiveness();
    this.generation++;
    for (const subscription of this.subscriptions) subscription.dispose();
    this.client?.dispose();
    this.cancelChanges();
    this.cancelValidation();
    for (const client of this.verifyClients) client.dispose({ immediate: true });
    this.verifyClients.clear();
    this.sessionMutations.dispose();
    this.client = undefined;
    this.state = { status: 'disposed' };
    this.emitter.dispose();
  }
}

module.exports = { BackendManager, normalizeWorkspacePath };
