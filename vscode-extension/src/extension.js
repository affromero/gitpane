'use strict';

const vscode = require('vscode');
const fs = require('node:fs');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { BackendManager } = require('./backend-manager');
const { GraphProvider } = require('./graph-provider');
const { BranchReviewProvider } = require('./branch-review-provider');
const { GithubProvider } = require('./github-provider');
const { SubmoduleProvider } = require('./submodule-provider');
const { SessionProvider } = require('./session-provider');
const { RepoTerminalProvider } = require('./repo-terminal-provider');
const { LauncherProvider } = require('./launcher-provider');
const { MutationManager } = require('./mutation-manager');
const { SidebarActions } = require('./sidebar-actions');
const { changedFileCount, comparison, groups, isSubmodule, statusLabel } = require('./model');

function directoryIdentity(value) {
  try {
    const real = fs.realpathSync.native(value);
    const stat = fs.statSync(real, { bigint: true });
    if (!stat.isDirectory()) return undefined;
    return { path: process.platform === 'win32' ? real.toLowerCase() : real, dev: stat.dev, ino: stat.ino };
  } catch { return undefined; }
}

function sameIdentity(a, b) {
  return !!a && !!b && a.path === b.path && a.dev === b.dev && a.ino === b.ino;
}

function inside(parent, child) {
  const relative = path.relative(parent, child);
  return relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

async function parallel(items, limit, action) {
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const item = items[next++];
      await action(item);
    }
  }));
}

function validBackendStatus(status) {
  const count = value => Number.isSafeInteger(value) && value >= 0;
  return status && typeof status.branch === 'string' && typeof status.is_dirty === 'boolean' &&
    count(status.ahead) && count(status.behind) && Array.isArray(status.stashes) &&
    Array.isArray(status.worktrees) && status.worktrees.every(item => item && typeof item.path === 'string' && path.isAbsolute(item.path)) &&
    typeof status.has_dirty_submodules === 'boolean' && typeof status.has_unpushed_submodules === 'boolean';
}

const SORT_ORDERS = new Set(['workspace', 'alphabetical', 'reverse', 'dirty']);

class RepositoryTree {
  constructor(git, output, backend, sortOrder = 'workspace') {
    this.git = git;
    this.output = output;
    this.backend = backend;
    this.sortOrder = SORT_ORDERS.has(sortOrder) ? sortOrder : 'workspace';
    this.sortGeneration = 0;
    this.sortTimer = undefined;
    this.sortRanks = new Map();
    this.sortError = undefined;
    this.emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.emitter.event;
    this.listeners = new Map();
    this.repositoryIdentities = new Map();
    this.workspaceIdentities = new Map();
    this.groupCache = new Map();
    this.backendStatuses = new Map();
    this.backendErrors = new Map();
    this.backendGeneration = 0;
    this.backendError = undefined;
    this.workspacePaths = [];
    this.changeTimer = undefined;
    this.pollError = undefined;
    this.failedStatuses = new Set();
    this.statusErrors = new Map();
    this.busy = false;
    this.pending = false;
    this.disposed = false;
    this.visible = false;
    this.pollTimer = undefined;
    this.pollPromise = undefined;
    this.refreshTimer = undefined;
    this.subscriptions = [
      git.onDidOpenRepository(repo => {
        this.observe(repo);
        if (this.isVisible(repo)) this.scheduleRefresh();
      }),
      git.onDidCloseRepository(repo => {
        const wasVisible = this.isVisible(repo);
        this.unobserve(repo);
        if (wasVisible) this.scheduleChanged();
      }),
      backend.onDidChange(state => this.backendChanged(state)),
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        this.backendGeneration++;
        this.backendStatuses.clear();
        this.backendErrors.clear();
        this.updateWorkspacePaths();
        this.changed();
        this.refresh().catch(error => {
          this.fail(`Could not refresh repository status: ${error.message}`);
          this.changed();
        });
      })
    ];
    this.updateWorkspacePaths();
    for (const repo of git.repositories) this.observe(repo);
    this.backendChanged(backend.state);
  }

  observe(repo) {
    const key = repo.rootUri.toString();
    if (this.listeners.has(key)) return;
    this.repositoryIdentities.set(key, directoryIdentity(repo.rootUri.fsPath));
    this.listeners.set(key, repo.state.onDidChange(() => {
      this.groupCache.delete(key);
      if (this.isVisible(repo)) this.scheduleChanged();
    }));
  }

  unobserve(repo) {
    const key = repo.rootUri.toString();
    this.listeners.get(key)?.dispose();
    this.listeners.delete(key);
    this.repositoryIdentities.delete(key);
    this.groupCache.delete(key);
    this.failedStatuses.delete(key);
    this.statusErrors.delete(key);
    this.backendStatuses.delete(key);
    this.backendErrors.delete(key);
  }

  backendChanged(state) {
    if (this.disposed) return;
    const wasUnavailable = this.backendError !== undefined;
    if (state.status === 'initializing') {
      this.backendGeneration++;
      this.backendStatuses.clear();
      this.backendErrors.clear();
      this.backendError = undefined;
    } else if (state.status === 'error') {
      this.backendError = `Gitpane backend unavailable: ${state.error?.message || 'Unknown error'}`;
      if (['service_exited', 'spawn_error', 'io_error', 'protocol_error', 'response_too_large', 'unsupported_version', 'invalid_backend_path'].includes(state.error?.code)) {
        this.backendGeneration++;
        this.backendStatuses.clear();
      }
    } else if (state.status === 'ready') {
      this.backendError = undefined;
      if (wasUnavailable && !this.busy) this.scheduleRefresh();
    }
    this.scheduleChanged();
  }

  async refreshBackend(repo) {
    const key = repo.rootUri.toString();
    const generation = this.backendGeneration;
    try {
      const status = await this.backend.status(repo.rootUri.fsPath);
      if (this.disposed || generation !== this.backendGeneration || !this.isVisible(repo)) return;
      if (!validBackendStatus(status)) throw new Error('Gitpane backend returned an invalid repository status.');
      this.backendStatuses.set(key, status);
      this.backendErrors.delete(key);
    } catch (error) {
      if (this.disposed || generation !== this.backendGeneration || !this.isVisible(repo) || error.code === 'workspace_changed') return;
      this.backendStatuses.delete(key);
      const message = `Could not load Gitpane status for ${repo.rootUri.fsPath}: ${error.message}`;
      if (this.backendErrors.get(key) !== message) this.output.appendLine(message);
      this.backendErrors.set(key, message);
    }
  }

  changed() {
    if (this.disposed) return;
    if (this.sortOrder !== 'workspace') this.scheduleSort();
    this.emitter.fire();
  }

  setSortOrder(order) {
    if (!SORT_ORDERS.has(order) || order === this.sortOrder) return;
    this.sortOrder = order;
    this.sortRanks.clear();
    this.sortError = undefined;
    this.sortGeneration++;
    this.changed();
  }

  scheduleSort() {
    this.sortGeneration++;
    if (this.sortTimer !== undefined) return;
    this.sortTimer = setTimeout(() => {
      this.sortTimer = undefined;
      this.refreshSort();
    }, 0);
  }

  async refreshSort() {
    if (this.disposed || this.sortOrder === 'workspace') return;
    const generation = this.sortGeneration;
    const order = this.sortOrder;
    const nodes = this.getChildren().filter(node => node.type === 'repo');
    try {
      const repositories = nodes.map(({ repo }) => {
        const identity = this.repositoryIdentities.get(repo.rootUri.toString());
        if (!identity) throw new Error('Repository identity is no longer available.');
        const roots = [...this.workspaceIdentities.values()].filter(root => root && inside(root.path, identity.path));
        roots.sort((a, b) => b.path.length - a.path.length);
        const display = roots.length ? path.relative(roots[0].path, identity.path) || path.basename(identity.path) : path.basename(identity.path);
        return { path: repo.rootUri.fsPath, display, dirty: this.groupsFor(repo).length > 0 || !!this.backendStatuses.get(repo.rootUri.toString())?.is_dirty };
      });
      const result = await this.backend.sortRepositories(order, repositories);
      if (this.disposed || generation !== this.sortGeneration || order !== this.sortOrder) return;
      const indices = result?.indices;
      if (!Array.isArray(indices) || indices.length !== nodes.length || new Set(indices).size !== nodes.length ||
          indices.some(index => !Number.isSafeInteger(index) || index < 0 || index >= nodes.length) ||
          nodes.some(node => !this.isVisible(node.repo))) throw new Error('Gitpane returned an invalid repository order.');
      this.sortRanks = new Map(indices.map((index, rank) => [nodes[index].repo.rootUri.toString(), rank]));
      this.sortError = undefined;
      this.emitter.fire();
    } catch (error) {
      if (this.disposed || generation !== this.sortGeneration || order !== this.sortOrder) return;
      this.sortError = `Could not sort repositories: ${error.message}`;
      this.output.appendLine(this.sortError);
      this.emitter.fire();
    }
  }

  scheduleChanged() {
    if (this.disposed || this.changeTimer !== undefined) return;
    this.changeTimer = setTimeout(() => {
      this.changeTimer = undefined;
      this.changed();
    }, 50);
  }

  scheduleRefresh() {
    if (this.disposed || this.refreshTimer !== undefined) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined;
      this.refresh().catch(error => {
        this.fail(`Could not refresh repository status: ${error.message}`);
        this.changed();
      });
    }, 100);
  }

  updateWorkspacePaths() {
    const folders = (vscode.workspace.workspaceFolders || []).filter(folder => folder.uri.scheme === 'file');
    const active = new Set(folders.map(folder => folder.uri.fsPath));
    for (const key of this.workspaceIdentities.keys()) if (!active.has(key)) this.workspaceIdentities.delete(key);
    for (const folder of folders) {
      const key = folder.uri.fsPath;
      if (!this.workspaceIdentities.has(key)) this.workspaceIdentities.set(key, directoryIdentity(key));
    }
    this.workspacePaths = folders.map(folder => folder.uri.fsPath);
  }

  isPathInWorkspace(value) {
    const target = directoryIdentity(value);
    return this.containsIdentity(target);
  }

  containsIdentity(target) {
    if (!target) return false;
    for (const [source, identity] of this.workspaceIdentities) {
      if (sameIdentity(identity, directoryIdentity(source)) && inside(identity.path, target.path)) return true;
    }
    return false;
  }

  groupsFor(repo) {
    const key = repo.rootUri.toString();
    if (!this.groupCache.has(key)) this.groupCache.set(key, groups(repo.state));
    return this.groupCache.get(key);
  }

  fail(message) {
    if (this.pollError !== message) this.output.appendLine(message);
    this.pollError = message;
  }

  statusFailed(repo, error) {
    if (this.disposed) return;
    const key = repo.rootUri.toString();
    const message = `Could not refresh ${repo.rootUri.fsPath}: ${error.message}`;
    if (this.statusErrors.get(key) !== message) this.output.appendLine(message);
    this.statusErrors.set(key, message);
    this.failedStatuses.add(key);
  }

  statusSucceeded(repo) {
    if (this.disposed) return;
    const key = repo.rootUri.toString();
    this.statusErrors.delete(key);
    this.failedStatuses.delete(key);
    this.groupCache.delete(key);
  }

  setVisible(visible) {
    this.visible = visible;
    if (!visible && this.pollTimer !== undefined) {
      clearTimeout(this.pollTimer);
      this.pollTimer = undefined;
    }
    if (visible) this.schedulePoll();
  }

  schedulePoll() {
    if (this.disposed || !this.visible || this.pollTimer !== undefined || this.pollPromise) return;
    this.pollTimer = setTimeout(() => {
      this.pollTimer = undefined;
      this.pollPromise = this.reconcile().catch(error => {
        this.fail(`Could not refresh repository status: ${error.message}`);
        this.changed();
      });
      this.pollPromise.finally(() => {
        this.pollPromise = undefined;
        this.schedulePoll();
      });
    }, 5000);
  }

  async reconcile() {
    if (this.busy || this.disposed || !this.visible) return;
    const repos = this.git.repositories.filter(repo => this.isVisible(repo));
    await parallel(repos, 4, async repo => {
      if (this.disposed || !this.visible || !this.isVisible(repo)) return;
      try {
        await repo.status();
        this.statusSucceeded(repo);
      } catch (error) {
        this.statusFailed(repo, error);
      }
      await this.refreshBackend(repo);
    });
    this.pollError = undefined;
    this.scheduleChanged();
  }

  isVisible(repo) {
    if (!repo || repo.rootUri.scheme !== 'file') return false;
    const identity = this.repositoryIdentities.get(repo.rootUri.toString());
    const current = directoryIdentity(repo.rootUri.fsPath);
    return sameIdentity(identity, current) && this.containsIdentity(current);
  }

  async refresh(retryBackend = false) {
    if (retryBackend && this.backend.state.status === 'error') {
      try { await this.backend.retry(); }
      catch { this.changed(); return; }
    }
    if (this.pollPromise) await this.pollPromise;
    if (this.disposed) return;
    if (this.busy) {
      this.pending = true;
      return;
    }
    this.busy = true;
    this.pollError = undefined;
    this.updateWorkspacePaths();
    this.changed();
    try {
      const repos = this.git.repositories.filter(repo => this.isVisible(repo));
      await parallel(repos, 4, async repo => {
        if (this.disposed || !this.isVisible(repo)) return;
        try {
          await repo.status();
          this.statusSucceeded(repo);
        }
        catch (error) {
          this.statusFailed(repo, error);
        }
        await this.refreshBackend(repo);
      });
      const active = new Set(repos.map(repo => repo.rootUri.toString()));
      for (const key of this.statusErrors.keys()) {
        if (!active.has(key)) {
          this.statusErrors.delete(key);
          this.failedStatuses.delete(key);
        }
      }
      for (const key of this.backendStatuses.keys()) if (!active.has(key)) this.backendStatuses.delete(key);
      for (const key of this.backendErrors.keys()) if (!active.has(key)) this.backendErrors.delete(key);
    } finally {
      this.busy = false;
      this.changed();
      if (this.pending && !this.disposed) {
        this.pending = false;
        await this.refresh();
      }
    }
  }

  getChildren(node) {
    if (!node) {
      const failures = [this.pollError, this.backendError, this.sortError, ...this.statusErrors.values(), ...this.backendErrors.values()].filter(Boolean).map(message => ({ type: 'error', message }));
      const repos = this.git.repositories.filter(repo => this.isVisible(repo)).map(repo => ({
        repo, dirty: this.groupsFor(repo).length > 0 || !!this.backendStatuses.get(repo.rootUri.toString())?.is_dirty
      })).sort((a, b) => Number(b.dirty) - Number(a.dirty) || a.repo.rootUri.fsPath.localeCompare(b.repo.rootUri.fsPath));
      if (this.sortOrder !== 'workspace' && this.sortRanks.size) {
        repos.sort((a, b) => (this.sortRanks.get(a.repo.rootUri.toString()) ?? Number.MAX_SAFE_INTEGER) -
          (this.sortRanks.get(b.repo.rootUri.toString()) ?? Number.MAX_SAFE_INTEGER));
      }
      if (this.busy && !repos.length && !failures.length) return [{ type: 'info', message: 'Loading repositories…' }];
      if (!repos.length && !failures.length) return [{ type: 'info', message: this.workspacePaths.length
        ? 'No repositories found in the workspace' : 'Open a folder or workspace to see repositories' }];
      const names = new Map();
      for (const { repo } of repos) {
        const name = path.basename(repo.rootUri.fsPath);
        names.set(name, (names.get(name) || 0) + 1);
      }
      return [...failures, ...repos.map(({ repo }) => ({ type: 'repo', repo,
        duplicate: names.get(path.basename(repo.rootUri.fsPath)) > 1 }))];
    }
    if (node.type === 'repo') return this.groupsFor(node.repo).map(group => ({ type: 'group', repo: node.repo, group }));
    if (node.type === 'group') return node.group.changes.map(change => ({ type: 'change', repo: node.repo, group: node.group.id, change }));
    return [];
  }

  dispose() {
    this.disposed = true;
    if (this.changeTimer !== undefined) clearTimeout(this.changeTimer);
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer);
    if (this.pollTimer !== undefined) clearTimeout(this.pollTimer);
    if (this.sortTimer !== undefined) clearTimeout(this.sortTimer);
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const subscription of this.listeners.values()) subscription.dispose();
    this.emitter.dispose();
  }
}

class GitpaneViewProvider {
  constructor(tree, extensionUri) {
    this.tree = tree;
    this.extensionUri = extensionUri;
    this.view = undefined;
    this.ready = false;
    this.sessionId = randomUUID();
    this.nextId = 0;
    this.ids = new Map();
    this.repositories = new Map();
    this.changes = new Map();
    this.worktrees = new Map();
    this.viewDisposables = [];
    this.subscription = tree.onDidChangeTreeData(() => this.publish());
  }

  idFor(key, live) {
    live.add(key);
    if (!this.ids.has(key)) this.ids.set(key, `item-${this.sessionId}-${++this.nextId}`);
    return this.ids.get(key);
  }

  snapshot() {
    const live = new Set();
    const changes = new Map();
    const worktrees = new Map();
    const repositoriesById = new Map();
    const children = this.tree.getChildren();
    const repositories = children.filter(node => node.type === 'repo').map(node => {
      const repo = node.repo;
      const root = repo.rootUri.fsPath;
      const repoKey = repo.rootUri.toString();
      const status = this.tree.backendStatuses.get(repoKey);
      const id = this.idFor(JSON.stringify(['repo', repoKey]), live);
      repositoriesById.set(id, repoKey);
      const head = repo.state.HEAD;
      const stashes = (status?.stashes || []).filter(item => item && Number.isSafeInteger(item.index) &&
        item.index >= 0 && typeof item.message === 'string' && typeof item.oid === 'string').map(item => ({
        id: this.idFor(JSON.stringify(['stash', repoKey, item.oid, item.index]), live),
        index: item.index, message: item.message
      }));
      const worktreeData = (status?.worktrees || []).filter(item => this.tree.isPathInWorkspace(item.path)).map(item => {
        const worktreeId = this.idFor(JSON.stringify(['worktree', repoKey, item.path]), live);
        worktrees.set(worktreeId, { repoKey, path: item.path });
        return { id: worktreeId, name: path.basename(item.path), branch: item.branch,
          dirty: item.is_dirty, fileCount: item.file_count, ahead: item.ahead, behind: item.behind };
      });
      const changeGroups = this.tree.getChildren(node);
      const groupData = changeGroups.map(groupNode => ({
        id: groupNode.group.id,
        label: groupNode.group.label,
        files: this.tree.getChildren(groupNode).map(fileNode => {
          const change = fileNode.change;
          const fileId = this.idFor(JSON.stringify(['file', repoKey, groupNode.group.id, change.uri.toString()]), live);
          changes.set(fileId, fileNode);
          return { id: fileId, name: path.basename(change.uri.fsPath),
            path: path.relative(root, change.uri.fsPath), status: statusLabel(change.status) };
        })
      }));
      return {
        id,
        name: node.duplicate ? `${path.basename(root)} (${path.dirname(root)})` : path.basename(root),
        branch: status?.branch || head?.name || (head?.commit ? head.commit.slice(0, 8) : 'unborn'),
        dirty: !!status?.is_dirty || changedFileCount(this.tree.groupsFor(repo)) > 0,
        ahead: status?.ahead || 0,
        behind: status?.behind || 0,
        worktree: repo.kind === 'worktree',
        statusUnavailable: this.tree.failedStatuses.has(repoKey) || !status,
        stashCount: status?.stashes.length || 0,
        stashes,
        worktreeCount: worktreeData.length,
        worktrees: worktreeData,
        hasDirtySubmodules: status?.has_dirty_submodules || false,
        hasUnpushedSubmodules: status?.has_unpushed_submodules || false,
        groups: groupData
      };
    });
    for (const key of this.ids.keys()) if (!live.has(key)) this.ids.delete(key);
    this.changes = changes;
    this.worktrees = worktrees;
    this.repositories = repositoriesById;
    return {
      type: 'snapshot', repositories,
      errors: children.filter(node => node.type === 'error').map(node => node.message),
      emptyMessage: children.find(node => node.type === 'info')?.message,
      loading: this.tree.busy
    };
  }

  publish() {
    if (!this.view || !this.ready) return;
    this.view.webview.postMessage(this.snapshot());
  }

  currentChange(node) {
    if (!node?.repo?.rootUri || !node.change?.uri || !this.tree.isVisible(node.repo)) return undefined;
    const repoKey = node.repo.rootUri.toString();
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === repoKey);
    if (!repo) return undefined;
    const group = this.tree.groupsFor(repo).find(item => item.id === node.group);
    const change = group?.changes.find(item => item.uri.toString() === node.change.uri.toString());
    return change ? { type: 'change', repo, group: node.group, change } : undefined;
  }

  currentRepository(id) {
    const repoKey = this.repositories.get(id);
    if (!repoKey) return undefined;
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === repoKey);
    return repo && this.tree.isVisible(repo) ? repo : undefined;
  }

  actionRepository(repo) {
    if (!repo || !this.tree.isVisible(repo)) return undefined;
    const status = this.tree.backendStatuses.get(repo.rootUri.toString());
    if (!status || typeof status.common_dir !== 'string' || !path.isAbsolute(status.common_dir)) return undefined;
    return { repoPath: repo.rootUri.fsPath, commonDir: status.common_dir,
      branch: status.branch === 'HEAD' ? null : status.branch, head: status.head_oid };
  }

  resolveRepository(id) {
    return this.actionRepository(this.currentRepository(id));
  }

  resolveChange(id) {
    const node = this.currentChange(this.changes.get(id));
    const repo = node && this.actionRepository(node.repo);
    if (!repo) return undefined;
    const relative = path.relative(repo.repoPath, node.change.uri.fsPath);
    if (!relative || path.isAbsolute(relative) || relative === '..' || relative.startsWith(`..${path.sep}`)) return undefined;
    return { ...repo, path: relative, group: node.group, status: node.change.status, uri: node.change.uri, node };
  }

  resolveWorktree(id) {
    const selected = this.worktrees.get(id);
    if (!selected) return undefined;
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === selected.repoKey);
    const actionRepo = this.actionRepository(repo);
    if (!actionRepo || !this.tree.isPathInWorkspace(selected.path)) return undefined;
    const status = this.tree.backendStatuses.get(selected.repoKey);
    const worktree = status.worktrees.find(item => item.path === selected.path);
    if (!worktree || typeof worktree.branch !== 'string') return undefined;
    return { ...actionRepo, path: worktree.path, branch: worktree.branch };
  }

  resolveWebviewView(view) {
    for (const disposable of this.viewDisposables) disposable.dispose();
    this.viewDisposables = [];
    this.view = view;
    this.ready = false;
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'src', 'webview')]
    };
    const { getHtml } = require('./webview/html');
    view.webview.html = getHtml(view.webview, this.extensionUri);
    this.tree.setVisible(view.visible);
    this.viewDisposables.push(
      view.webview.onDidReceiveMessage(async message => {
        if (message?.type === 'ready') {
          this.ready = true;
          this.publish();
        } else if (message?.type === 'refresh') {
          try { await this.tree.refresh(true); }
          catch (error) {
            this.tree.fail(`Could not refresh repository status: ${error.message}`);
            this.tree.changed();
          }
        } else if (message?.type === 'openChange' && typeof message.id === 'string') {
          const node = this.currentChange(this.changes.get(message.id));
          if (node) await vscode.commands.executeCommand('gitpane.openChange', node);
        } else if (message?.type === 'openGraph' && typeof message.id === 'string') {
          const repo = this.currentRepository(message.id);
          if (repo) await vscode.commands.executeCommand('gitpane.openGraph', repo);
        } else if (message?.type === 'menu' && typeof message.id === 'string') {
          await this.actions?.showMenu(message.kind, message.id);
        }
      }),
      view.onDidChangeVisibility(() => this.tree.setVisible(view.visible)),
      view.onDidDispose(() => {
        if (this.view !== view) return;
        this.tree.setVisible(false);
        this.view = undefined;
        this.ready = false;
        this.repositories.clear();
        this.changes.clear();
        this.worktrees.clear();
      })
    );
  }

  dispose() {
    this.tree.setVisible(false);
    this.subscription.dispose();
    for (const disposable of this.viewDisposables) disposable.dispose();
    this.viewDisposables = [];
    this.view = undefined;
    this.repositories.clear();
    this.changes.clear();
    this.worktrees.clear();
  }
}

async function activate(context) {
  const output = vscode.window.createOutputChannel('Gitpane');
  context.subscriptions.push(output);
  const gitExtension = vscode.extensions.getExtension('vscode.git');
  if (!gitExtension) {
    vscode.window.showErrorMessage('Gitpane requires the built-in Git extension.');
    return;
  }
  const git = await gitExtension.activate();
  if (!git.enabled) {
    vscode.window.showErrorMessage('Gitpane requires the built-in Git extension to be enabled.');
    return;
  }
  const api = git.getAPI(1);
  const backend = new BackendManager(context, { output });
  const tree = new RepositoryTree(api, output, backend, context.workspaceState?.get('gitpane.sortOrder'));
  const provider = new GitpaneViewProvider(tree, context.extensionUri);
  const graph = new GraphProvider(tree, backend, api, context.extensionUri);
  const review = new BranchReviewProvider(tree, backend, api);
  const github = new GithubProvider(tree, backend);
  const sessions = new SessionProvider(tree, backend);
  const terminals = new RepoTerminalProvider(tree);
  const launchers = new LauncherProvider(tree, backend);
  const submodules = new SubmoduleProvider(tree, backend, api, {
    resolveChange: node => provider.currentChange(node),
    openGraph: repo => graph.open(repo)
  });
  const mutations = new MutationManager({
    getSnapshot: () => backend.mutationSnapshot(),
    isCurrent: (snapshot, repo) => backend.isCurrentMutation(snapshot, repo),
    onStderr: text => output.append(text)
  });
  provider.actions = new SidebarActions({ vscode, mutations,
    previewFileAction: (repo, target, operation) => backend.fileActionPreview(repo, target, operation),
    resolveRepository: id => provider.resolveRepository(id),
    resolveChange: id => provider.resolveChange(id),
    resolveWorktree: id => provider.resolveWorktree(id),
    reviewBranch: async id => {
      const repo = provider.currentRepository(id);
      if (repo) await review.open(repo);
    },
    openGithub: async id => {
      const repo = provider.currentRepository(id);
      if (repo) await github.open(repo);
    },
    openSession: async id => {
      const repo = provider.currentRepository(id);
      if (repo) await sessions.open(repo);
    },
    openTerminal: async id => {
      const repo = provider.currentRepository(id);
      if (repo) await terminals.open(repo);
    },
    openLauncher: async id => {
      const repo = provider.currentRepository(id);
      if (repo) await launchers.open(repo);
    },
    onWorktreeCreated: async target => {
      if (!tree.isPathInWorkspace(target)) return;
      try {
        const opened = await api.openRepository(vscode.Uri.file(target));
        if (!opened) throw new Error('VS Code did not open the repository.');
      }
      catch (error) { throw new Error(`Worktree created, but VS Code could not open it: ${error.message}`); }
    },
    refresh: () => tree.refresh() });
  context.subscriptions.push(backend, tree, provider, graph, review, github, sessions, terminals, launchers, submodules, mutations,
    vscode.window.registerWebviewViewProvider('gitpane.repositories', provider),
    vscode.workspace.registerTextDocumentContentProvider('gitpane-empty', { provideTextDocumentContent: () => '' }),
    vscode.commands.registerCommand('gitpane.refresh', () => tree.refresh(true)),
    vscode.commands.registerCommand('gitpane.sort', async () => {
      const choices = [
        { label: 'Default (dirty, by path)', order: 'workspace' },
        { label: 'A–Z', order: 'alphabetical' },
        { label: 'Z–A', order: 'reverse' },
        { label: 'Dirty first (Gitpane)', order: 'dirty' }
      ];
      const selected = await vscode.window.showQuickPick(choices, { placeHolder: 'Sort Gitpane repositories' });
      if (!selected || !choices.includes(selected)) return;
      try {
        await context.workspaceState?.update('gitpane.sortOrder', selected.order);
        tree.setSortOrder(selected.order);
      } catch (error) { vscode.window.showErrorMessage(`Gitpane could not save the sort order: ${error.message}`); }
    }),
    vscode.commands.registerCommand('gitpane.openGraph', repo => {
      try { graph.open(repo); }
      catch (error) { vscode.window.showErrorMessage(`Gitpane could not open the graph: ${error.message}`); }
    }),
    vscode.commands.registerCommand('gitpane.openChange', async selected => {
      try {
        let node = provider.currentChange(selected);
        if (!node) throw new Error('The change is no longer in the workspace.');
        if (isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules)) {
          await submodules.open(node);
          return;
        }
        const plan = comparison(node.group, node.change);
        if (plan.open) {
          try { await vscode.workspace.fs.stat(plan.open); }
          catch (error) { throw new Error(`Conflict file is unavailable: ${error.message}`); }
          node = provider.currentChange(node);
          if (!node) throw new Error('The change is no longer in the workspace.');
          await vscode.commands.executeCommand('vscode.open', plan.open);
          return;
        }
        const empty = node.change.uri.with({ scheme: 'gitpane-empty', query: '' });
        const resolve = side => side.empty ? empty : side.file || api.toGitUri(side.git, side.ref);
        const title = `${path.basename(node.change.uri.fsPath)} (${node.group})`;
        await vscode.commands.executeCommand('vscode.diff', resolve(plan.left), resolve(plan.right), title);
      } catch (error) {
        output.appendLine(`Could not open change: ${error.message}`);
        vscode.window.showErrorMessage(`Gitpane could not open the change: ${error.message}`);
      }
    })
  );
  await tree.refresh();
}

module.exports = { activate, GitpaneViewProvider, RepositoryTree };
